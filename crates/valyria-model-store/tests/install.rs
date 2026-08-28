//! End-to-end coverage of the install flow against the in-memory fetcher:
//! plan → confirm → resumable download → integrity check → probe →
//! manifest, plus removal and GC.

use std::collections::BTreeMap;
use std::sync::Arc;

use valyria_hardware::report::{CpuInfo, DiskInfo, HardwareReport};
use valyria_hardware::{Fit, ModelRequirement};
use valyria_model::SamplingParams;
use valyria_model_registry::{ModelCard, ModelRole, Quantization, TransportPreference};
use valyria_model_store::{
    InMemoryFetcher, InstalledModelStore, ModelStore, ModelStoreError, NullProber, MIGRATIONS,
};
use valyria_store::Store;
use valyria_util::{CancellationToken, ContentHash};

const URL: &str = "https://example.test/weights.gguf";

fn weights_bytes() -> Vec<u8> {
    // ~200 KiB so the 8 MiB chunk loop runs once, and small enough to keep
    // the test fast.
    (0..200_000u32).map(|i| (i % 251) as u8).collect()
}

fn card_with_hash(hash: String, size: u64) -> ModelCard {
    let mut role_suitability = BTreeMap::new();
    role_suitability.insert(ModelRole::PrimaryCoder, 90);
    ModelCard {
        id: "test-model-7b-q4".into(),
        family: "test".into(),
        display_name: "Test Model".into(),
        parameters_b: 7.0,
        quantization: Quantization::Q4KM,
        context_length: 8192,
        file_size_bytes: size,
        chat_template: None,
        recommended_sampling: SamplingParams::default(),
        role_suitability,
        requirement: ModelRequirement {
            min_ram_bytes: 6_000_000_000,
            min_vram_bytes: None,
        },
        transport_preference: TransportPreference::Native,
        supports_native_tools: true,
        supports_grammar: true,
        source_url: URL.into(),
        content_hash: hash,
        license_name: "Apache-2.0".into(),
        license_url: Some("https://example.test/license".into()),
    }
}

fn good_card() -> ModelCard {
    let bytes = weights_bytes();
    card_with_hash(ContentHash::of_bytes(&bytes).to_hex(), bytes.len() as u64)
}

fn hw(ram_available: u64) -> HardwareReport {
    HardwareReport {
        os: "test".into(),
        os_version: None,
        arch: "test".into(),
        cpu: CpuInfo {
            brand: "test".into(),
            physical_cores: 8,
            logical_cores: 16,
            arch: "test".into(),
        },
        ram_total_bytes: ram_available * 2,
        ram_available_bytes: ram_available,
        gpus: vec![],
        unified_memory: true,
        accelerator_present: None,
        disk: DiskInfo {
            total_bytes: 0,
            available_bytes: 0,
        },
    }
}

#[tokio::test]
async fn plan_surfaces_size_license_and_fit() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();

    let plan = store.plan_install(&card, &hw(64_000_000_000));
    assert_eq!(plan.download_bytes, card.file_size_bytes);
    assert_eq!(plan.license_name, "Apache-2.0");
    assert_eq!(
        plan.license_url.as_deref(),
        Some("https://example.test/license")
    );
    assert_eq!(plan.fit, Fit::Comfortable);
    assert!(!plan.already_installed);
    assert!(!plan.is_confirmed());

    // Tiny machine: the plan still forms, but the fit says it won't run.
    let tight = store.plan_install(&card, &hw(3_000_000_000));
    assert!(matches!(tight.fit, Fit::WillNotFit { .. }));
}

#[tokio::test]
async fn unconfirmed_plan_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());

    let plan = store.plan_install(&card, &hw(64_000_000_000));
    let err = store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ModelStoreError::Unconfirmed { .. }));
}

#[tokio::test]
async fn happy_path_downloads_verifies_probes_and_writes_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());

    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();
    let manifest = store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(manifest.card.id, card.id);
    assert_eq!(manifest.size_bytes, weights_bytes().len() as u64);
    assert_eq!(manifest.content_hash, card.content_hash);
    assert!(manifest.probe.is_some());

    assert!(store.is_installed(&card.id));
    assert_eq!(store.installed().unwrap(), vec![card.id.clone()]);
    // Manifest round-trips from disk.
    assert_eq!(store.manifest(&card.id).unwrap(), manifest);
    // Integrity check passes on the freshly written file.
    store.verify_integrity(&card.id).unwrap();
}

#[tokio::test]
async fn integrity_mismatch_deletes_the_download_and_leaves_nothing_installed() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let bytes = weights_bytes();
    let card = card_with_hash("00".repeat(32), bytes.len() as u64); // wrong hash
    let fetcher = InMemoryFetcher::new().with_object(URL, bytes);

    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();
    let err = store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ModelStoreError::IntegrityMismatch { .. }));
    assert!(!store.is_installed(&card.id));

    let weights = store.models_dir().join(&card.id).join("weights.gguf");
    assert!(!weights.exists(), "verified-bad weights must be deleted");
    assert!(!weights.with_extension("gguf.part").exists());
}

#[tokio::test]
async fn interrupted_download_resumes_from_the_part_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let total = card.file_size_bytes;
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());
    fetcher.fail_once_after(total / 3);

    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();

    // First attempt: the fetcher fails a third of the way in.
    let err = store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ModelStoreError::Download { .. }));
    assert!(!store.is_installed(&card.id));

    // Second attempt: resumes from the `.part` and completes.
    store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();
    assert!(store.is_installed(&card.id));

    // The resume actually saved work: total bytes served is less than two
    // full downloads.
    assert!(
        fetcher.bytes_served() < 2 * total,
        "served {} for a {}-byte file — looks like a full re-download",
        fetcher.bytes_served(),
        total
    );
    assert!(fetcher.bytes_served() >= total);
}

#[tokio::test]
async fn cancel_leaves_a_part_file_and_a_later_run_completes() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());
    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();

    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = store
        .install(&plan, &fetcher, &NullProber, &cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, ModelStoreError::Cancelled { .. }));
    assert!(!store.is_installed(&card.id));

    store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();
    assert!(store.is_installed(&card.id));
}

#[tokio::test]
async fn already_installed_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());
    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();

    store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();
    let again = store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(again, ModelStoreError::AlreadyInstalled { .. }));
}

#[tokio::test]
async fn remove_reports_freed_bytes_and_uninstalls() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());
    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();
    store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();

    let freed = store.remove(&card.id).unwrap();
    assert!(freed >= weights_bytes().len() as u64);
    assert!(!store.is_installed(&card.id));
    assert!(matches!(
        store.remove(&card.id),
        Err(ModelStoreError::NotInstalled { .. })
    ));
}

#[tokio::test]
async fn gc_removes_models_not_in_keep_and_sweeps_partials() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());
    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();
    store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();

    // A stray partial from an interrupted unrelated download.
    let stray_dir = store.models_dir().join("ghost-model");
    std::fs::create_dir_all(&stray_dir).unwrap();
    std::fs::write(stray_dir.join("weights.gguf.part"), vec![0u8; 4096]).unwrap();

    let report = store.gc(&[]).unwrap();
    assert_eq!(report.removed, vec![card.id.clone()]);
    assert!(report.freed_bytes >= weights_bytes().len() as u64);
    assert!(report.swept_partials >= 4096);
    assert!(store.installed().unwrap().is_empty());

    let keep_report = store.gc(&["something".into()]).unwrap();
    assert!(keep_report.removed.is_empty());
}

#[tokio::test]
async fn storage_report_counts_models_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    assert_eq!(store.storage_report().unwrap().model_count, 0);

    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());
    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();
    store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();

    let report = store.storage_report().unwrap();
    assert_eq!(report.model_count, 1);
    assert!(report.total_bytes >= weights_bytes().len() as u64);
}

#[tokio::test]
async fn installed_model_db_index_records_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path());
    let card = good_card();
    let fetcher = InMemoryFetcher::new().with_object(URL, weights_bytes());
    let plan = store.plan_install(&card, &hw(64_000_000_000)).confirm();
    let manifest = store
        .install(&plan, &fetcher, &NullProber, &CancellationToken::new())
        .await
        .unwrap();

    let db = InstalledModelStore::new(Arc::new(Store::open_in_memory(MIGRATIONS).unwrap()));
    db.record(&manifest).await.unwrap();

    let row = db.get(&card.id).await.unwrap().expect("row present");
    assert_eq!(row.id, card.id);
    assert_eq!(row.content_hash, manifest.content_hash);
    assert_eq!(row.license_name, "Apache-2.0");
    assert!(row.probe_json.is_some());

    assert_eq!(db.list().await.unwrap().len(), 1);
    db.delete(&card.id).await.unwrap();
    assert!(db.get(&card.id).await.unwrap().is_none());
}
