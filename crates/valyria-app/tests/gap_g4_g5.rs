//! Integration coverage for hardware + model management added in protocol
//! 1.3.0 (CORE-INTERFACE gaps G4, G5): `hardware_probe`, `model_recommend`,
//! `model_install` (+ `model_install_*` events), `model_remove`,
//! `model_activate`, `model_inspect` over `EmbeddedClient`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{
    Client as _, Empty, ModelActivateRequest, ModelIdRequest, ModelRecommendRequest, Request,
    Response,
};

/// A catalog id known to exist in `Catalog::embedded()`.
const CATALOG_ID: &str = "qwen2.5-coder-7b-instruct-q4_k_m";

async fn runtime() -> (Arc<Runtime>, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let config = RuntimeConfig::new(ws.path()).with_data_dir(temp.path().join("data"));
    std::mem::forget(ws);
    (Arc::new(Runtime::open(config).await.unwrap()), temp)
}

#[tokio::test]
async fn hardware_probe_returns_a_structured_report() {
    let (rt, _t) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let Response::HardwareProbe(h) = client.call(Request::HardwareProbe(Empty {})).await else {
        panic!("expected HardwareProbe");
    };
    assert!(!h.os.is_empty());
    assert!(!h.arch.is_empty());
    assert!(!h.cpu.brand.is_empty());
    assert!(h.cpu.logical_cores >= 1);
    assert!(h.ram_total_bytes > 0);
}

#[tokio::test]
async fn model_recommend_scores_candidates_and_explains_the_pick() {
    let (rt, _t) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let Response::ModelRecommend(r) = client
        .call(Request::ModelRecommend(ModelRecommendRequest {
            role: "primary_coder".into(),
        }))
        .await
    else {
        panic!("expected ModelRecommend");
    };
    assert_eq!(r.role, "primary_coder");
    assert!(
        !r.candidates.is_empty(),
        "the catalog has primary_coder candidates"
    );
    for c in &r.candidates {
        assert!(["comfortable", "tight", "will_not_fit"].contains(&c.fit_kind.as_str()));
        assert!(!c.license_name.is_empty());
        assert!(c.size_bytes > 0);
    }
    // Fitting candidates carry a score and sort ahead of non-fitting ones.
    let first_non_fit = r
        .candidates
        .iter()
        .position(|c| c.fit_kind == "will_not_fit");
    let last_fit = r
        .candidates
        .iter()
        .rposition(|c| c.fit_kind != "will_not_fit");
    if let (Some(nf), Some(lf)) = (first_non_fit, last_fit) {
        assert!(lf < nf, "non-fitting candidates must sort last");
    }
    if let Some(rec) = &r.recommended {
        assert!(rec.adjusted_score.is_some());
        assert!(rec.suitability > 0);
        assert!(r.candidates.iter().any(|c| c.id == rec.id));
    }
}

#[tokio::test]
async fn model_recommend_rejects_an_unknown_role() {
    let (rt, _t) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let Response::Error(e) = client
        .call(Request::ModelRecommend(ModelRecommendRequest {
            role: "oracle".into(),
        }))
        .await
    else {
        panic!("expected an error");
    };
    assert_eq!(e.code, "model.unknown_role");
}

#[tokio::test]
async fn model_inspect_reports_card_detail_for_an_uninstalled_model() {
    let (rt, _t) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let Response::ModelInspect(m) = client
        .call(Request::ModelInspect(ModelIdRequest {
            id: CATALOG_ID.into(),
        }))
        .await
    else {
        panic!("expected ModelInspect");
    };
    assert_eq!(m.id, CATALOG_ID);
    assert!(!m.installed);
    assert!(m.installed_at_ms.is_none());
    assert!(m.active_roles.is_empty());
    assert!(m.parameters_b > 0.0);
    assert!(!m.source_url.is_empty());

    let Response::Error(e) = client
        .call(Request::ModelInspect(ModelIdRequest { id: "nope".into() }))
        .await
    else {
        panic!("expected an error for an unknown id");
    };
    assert_eq!(e.code, "app.repo");
}

#[tokio::test]
async fn remove_and_activate_reject_a_model_that_is_not_installed() {
    let (rt, _t) = runtime().await;
    let client = EmbeddedClient::new(rt);

    let Response::Error(e) = client
        .call(Request::ModelRemove(ModelIdRequest {
            id: CATALOG_ID.into(),
        }))
        .await
    else {
        panic!("expected an error");
    };
    assert_eq!(e.code, "model_store.not_installed");

    let Response::Error(e) = client
        .call(Request::ModelActivate(ModelActivateRequest {
            id: CATALOG_ID.into(),
            role: "primary_coder".into(),
        }))
        .await
    else {
        panic!("expected an error");
    };
    assert_eq!(e.code, "model_store.not_installed");
}

#[tokio::test]
async fn install_returns_immediately_and_reports_failure_on_the_event_stream() {
    let (rt, _t) = runtime().await;
    let client = EmbeddedClient::new(rt.clone());

    let mut stream = client.subscribe_events(0).await;

    // An empty in-memory fetcher: HEAD fails, so the background install
    // task fails fast and emits `model_install_failed` — exercising the
    // full Ack → spawn → channel → event path with no network.
    rt.model_install_with(CATALOG_ID, valyria_model_store::InMemoryFetcher::new())
        .await
        .expect("model_install_with returns immediately");

    let mut saw_failed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
            Ok(Some(ev)) if ev.kind == "model_install_failed" => {
                assert_eq!(
                    ev.payload.get("id").and_then(|v| v.as_str()),
                    Some(CATALOG_ID)
                );
                assert_eq!(
                    ev.payload.get("code").and_then(|v| v.as_str()),
                    Some("model_store.download")
                );
                saw_failed = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert!(saw_failed, "expected a model_install_failed event");

    // And an unknown catalog id is rejected synchronously.
    let Response::Error(e) = client
        .call(Request::ModelInstall(ModelIdRequest {
            id: "not-a-real-model".into(),
        }))
        .await
    else {
        panic!("expected a synchronous error");
    };
    assert_eq!(e.code, "app.repo");
}
