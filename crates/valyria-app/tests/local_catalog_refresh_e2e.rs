//! Real HTTP, not `InMemoryFetcher`: proves `Runtime::catalog_refresh`
//! (the production entry point — a real `HttpFetcher`, not the injected-
//! fetcher test seam `runtime.rs`'s offline tests use) actually works
//! against a real TCP connection to a real local HTTP server serving a
//! real signed catalog file.
//!
//! `#[ignore]`d — spawns a real `python3 -m http.server` subprocess, so
//! it needs Python 3 on `PATH` (present on every machine this workspace
//! targets, including CI):
//!
//! ```text
//! cargo test -p valyria-app --test local_catalog_refresh_e2e -- --ignored --nocapture
//! ```

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use valyria_app::{Runtime, RuntimeConfig};
use valyria_model_registry::{generate_keypair, sign};

struct Killed(Child);
impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
#[ignore]
async fn catalog_refresh_works_over_a_real_http_connection() {
    let key = generate_keypair();
    let json = r#"{"version":2,"models":[
        {"id":"refreshed-model","family":"f","display_name":"Refreshed","parameters_b":1.0,
         "quantization":"q4_k_m","context_length":2048,"file_size_bytes":1,
         "recommended_sampling":{"temperature":0.2,"top_p":0.9,"max_tokens":null,"stop":[]},
         "requirement":{"min_ram_bytes":1,"min_vram_bytes":null},
         "transport_preference":"native","supports_native_tools":true,"supports_grammar":false,
         "source_url":"u","content_hash":"aa","license_name":"MIT"}
    ]}"#;
    let sig = sign(&key, json.as_bytes());

    let serve_dir = tempfile::tempdir().unwrap();
    std::fs::write(serve_dir.path().join("catalog.json"), json).unwrap();
    std::fs::write(serve_dir.path().join("catalog.json.sig"), &sig).unwrap();

    let port = free_port();
    let child = Command::new("python3")
        .args([
            "-m",
            "http.server",
            &port.to_string(),
            "--bind",
            "127.0.0.1",
            "--directory",
        ])
        .arg(serve_dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a real python3 http.server");
    let _guard = Killed(child);

    let base = format!("http://127.0.0.1:{port}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let client = reqwest::Client::new();
    loop {
        if client
            .get(format!("{base}/catalog.json"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "hand-spawned http.server never came up"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let pubkey_hex: String = key
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path())
        .with_data_dir(temp.path().join("data"))
        .with_catalog_trusted_key_hex(pubkey_hex);
    let runtime = Runtime::open(config).await.unwrap();

    let outcome = runtime
        .catalog_refresh(
            &format!("{base}/catalog.json"),
            &format!("{base}/catalog.json.sig"),
        )
        .await
        .expect("a real HTTP fetch + real signature verification must succeed");
    assert_eq!(outcome.previous_version, 1);
    assert_eq!(outcome.new_version, 2);
    assert_eq!(outcome.model_count, 1);

    let models = runtime.model_list().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].card.id, "refreshed-model");
    println!("catalog_refresh over real HTTP: {outcome:?}");
}
