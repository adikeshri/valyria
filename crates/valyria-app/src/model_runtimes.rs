//! [`ModelRuntimeRegistry`]: owns the live local model-server handles
//! this `Runtime` started (`llama-server`, `mlx_lm.server`, ...), keyed
//! by the role each one serves. The orchestrator holds an `Arc<dyn
//! ModelRuntime>` *view* of the same object — this registry is the only
//! thing allowed to call `shutdown` on it, and the only place that knows
//! which catalog model id is behind a role right now (for
//! `model_remove`'s "which servers does this touch?" query). Engine-
//! agnostic by design: it holds `Arc<dyn LocalModelServer>`, the trait
//! every local engine adapter implements, and never needs to know which
//! one is actually running.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use valyria_model::LocalModelServer;
use valyria_orchestrator::Role;

struct LiveModel {
    model_id: String,
    handle: Arc<dyn LocalModelServer>,
}

#[derive(Default)]
pub struct ModelRuntimeRegistry {
    inner: Mutex<HashMap<Role, LiveModel>>,
}

impl ModelRuntimeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install `handle` as the server for `role`, returning whatever was
    /// there before so the caller can shut it down — **after** rebinding
    /// the orchestrator away from it, never before, or a request already
    /// in flight against it would find nothing to answer.
    pub async fn swap(
        &self,
        role: Role,
        model_id: String,
        handle: Arc<dyn LocalModelServer>,
    ) -> Option<Arc<dyn LocalModelServer>> {
        self.inner
            .lock()
            .await
            .insert(role, LiveModel { model_id, handle })
            .map(|prev| prev.handle)
    }

    pub async fn take(&self, role: Role) -> Option<Arc<dyn LocalModelServer>> {
        self.inner
            .lock()
            .await
            .remove(&role)
            .map(|prev| prev.handle)
    }

    /// Every role currently served by `model_id` — `model_remove` stops
    /// all of them before deleting the weights out from under them.
    pub async fn roles_for_model(&self, model_id: &str) -> Vec<Role> {
        self.inner
            .lock()
            .await
            .iter()
            .filter(|(_, m)| m.model_id == model_id)
            .map(|(role, _)| *role)
            .collect()
    }

    /// Best-effort stop of every running server — `Runtime` shutdown.
    pub async fn shutdown_all(&self) {
        let handles: Vec<_> = self
            .inner
            .lock()
            .await
            .drain()
            .map(|(_, m)| m.handle)
            .collect();
        for handle in handles {
            handle.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream, StreamExt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use valyria_model::{
        Capabilities, Chunk, Completion, GenerateRequest, Health, ModelError, ModelRuntime,
    };
    use valyria_util::CancellationToken;

    struct FakeServer {
        shutdowns: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelRuntime for FakeServer {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                context_length: 4096,
                supports_native_tools: true,
                supports_grammar: false,
                supports_streaming: false,
            }
        }
        async fn health(&self) -> Health {
            Health::Healthy
        }
        fn count_tokens(&self, text: &str) -> usize {
            text.len()
        }
        async fn generate(
            &self,
            _req: GenerateRequest,
            _cancel: CancellationToken,
        ) -> Result<Completion, ModelError> {
            unimplemented!()
        }
        fn stream(
            &self,
            _req: GenerateRequest,
            _cancel: CancellationToken,
        ) -> BoxStream<'static, Result<Chunk, ModelError>> {
            stream::empty().boxed()
        }
    }

    #[async_trait]
    impl LocalModelServer for FakeServer {
        async fn shutdown(&self) {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
        fn model_id(&self) -> &str {
            "fake"
        }
        fn port(&self) -> u16 {
            0
        }
    }

    fn server() -> (Arc<FakeServer>, Arc<AtomicUsize>) {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(FakeServer {
                shutdowns: shutdowns.clone(),
            }),
            shutdowns,
        )
    }

    #[tokio::test]
    async fn swap_returns_the_previous_handle_for_the_role() {
        let registry = ModelRuntimeRegistry::new();
        let (a, _) = server();
        let (b, _) = server();
        assert!(registry
            .swap(Role::PrimaryCoder, "a".into(), a)
            .await
            .is_none());
        let prev = registry.swap(Role::PrimaryCoder, "b".into(), b).await;
        assert!(prev.is_some());
    }

    #[tokio::test]
    async fn take_removes_and_returns() {
        let registry = ModelRuntimeRegistry::new();
        let (a, _) = server();
        registry.swap(Role::PrimaryCoder, "a".into(), a).await;
        assert!(registry.take(Role::PrimaryCoder).await.is_some());
        assert!(registry.take(Role::PrimaryCoder).await.is_none());
    }

    #[tokio::test]
    async fn roles_for_model_finds_every_role_serving_it() {
        let registry = ModelRuntimeRegistry::new();
        let (a1, _) = server();
        let (a2, _) = server();
        let (b, _) = server();
        registry.swap(Role::PrimaryCoder, "shared".into(), a1).await;
        registry.swap(Role::FastCoder, "shared".into(), a2).await;
        registry.swap(Role::Planner, "other".into(), b).await;

        let mut roles = registry.roles_for_model("shared").await;
        roles.sort();
        assert_eq!(roles, vec![Role::PrimaryCoder, Role::FastCoder]);
    }

    #[tokio::test]
    async fn shutdown_all_stops_every_server_exactly_once() {
        let registry = ModelRuntimeRegistry::new();
        let (a, a_shutdowns) = server();
        let (b, b_shutdowns) = server();
        registry.swap(Role::PrimaryCoder, "a".into(), a).await;
        registry.swap(Role::FastCoder, "b".into(), b).await;

        registry.shutdown_all().await;

        assert_eq!(a_shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(b_shutdowns.load(Ordering::SeqCst), 1);
        assert!(registry.take(Role::PrimaryCoder).await.is_none());
    }
}
