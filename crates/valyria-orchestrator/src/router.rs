//! Role routing with fallback chains (§38). A [`RoleBinding`] names a
//! primary model and an ordered list of fallbacks; [`RoleRouter::generate`]
//! walks that chain, skipping a model that has no registered runtime or
//! reports itself unavailable, and retrying the next one on a *retryable*
//! model error. A non-retryable model error is surfaced immediately — a
//! fallback chain is for "this model is down", not "this request is bad".

use std::collections::HashMap;
use std::sync::Arc;

use valyria_model::{Completion, GenerateRequest, Health, ModelRuntime};
use valyria_types::ErrorCode;
use valyria_util::CancellationToken;

use crate::error::{OrchestratorError, Result};
use crate::role::Role;

pub use valyria_model_registry::RoleBinding;

/// A completion plus which model in the chain actually produced it — the
/// caller records this so "which model did this work?" is answerable.
#[derive(Debug, Clone)]
pub struct RoutedCompletion {
    pub model_id: String,
    pub completion: Completion,
}

#[derive(Default)]
pub struct RoleRouter {
    runtimes: HashMap<String, Arc<dyn ModelRuntime>>,
    bindings: HashMap<Role, RoleBinding>,
}

impl std::fmt::Debug for RoleRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleRouter")
            .field("runtimes", &self.runtimes.keys().collect::<Vec<_>>())
            .field("bindings", &self.bindings.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl RoleRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make a concrete model runtime available under its catalog id.
    pub fn register(
        &mut self,
        model_id: impl Into<String>,
        runtime: Arc<dyn ModelRuntime>,
    ) -> &mut Self {
        self.runtimes.insert(model_id.into(), runtime);
        self
    }

    pub fn bind(&mut self, binding: RoleBinding) -> &mut Self {
        self.bindings.insert(binding.role, binding);
        self
    }

    pub fn binding(&self, role: Role) -> Option<&RoleBinding> {
        self.bindings.get(&role)
    }

    pub async fn generate(
        &self,
        role: Role,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<RoutedCompletion> {
        let binding = self
            .bindings
            .get(&role)
            .ok_or(OrchestratorError::NoBinding { role })?;

        let mut last = "no candidate model was reachable".to_string();
        for id in binding.chain() {
            let Some(runtime) = self.runtimes.get(id) else {
                last = format!("no runtime registered for {id:?}");
                continue;
            };
            if let Health::Unavailable { reason } = runtime.health().await {
                last = format!("{id:?} unavailable: {reason}");
                continue;
            }
            match runtime.generate(req.clone(), cancel.child()).await {
                Ok(completion) => {
                    return Ok(RoutedCompletion {
                        model_id: id.to_string(),
                        completion,
                    })
                }
                Err(e) if e.retryable() => {
                    tracing::warn!(model = %id, error = %e, "falling back to next model in chain");
                    last = e.to_string();
                    continue;
                }
                Err(e) => return Err(OrchestratorError::Model(e)),
            }
        }
        Err(OrchestratorError::AllFallbacksFailed { role, last })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use valyria_model::Message;
    use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};

    fn finishing_fake(summary: &str) -> Arc<dyn ModelRuntime> {
        Arc::new(FakeModelRuntime::from_scenario(Scenario {
            name: "ok".into(),
            turns: vec![ScriptedTurn::Finish {
                summary: summary.into(),
            }],
        }))
    }

    /// A runtime that is always `Unavailable` — stands in for a server
    /// that is down.
    struct DownRuntime;
    #[async_trait::async_trait]
    impl ModelRuntime for DownRuntime {
        fn capabilities(&self) -> valyria_model::Capabilities {
            valyria_model::Capabilities {
                context_length: 8192,
                supports_native_tools: true,
                supports_grammar: false,
                supports_streaming: true,
            }
        }
        async fn health(&self) -> Health {
            Health::Unavailable {
                reason: "connection refused".into(),
            }
        }
        fn count_tokens(&self, t: &str) -> usize {
            t.len()
        }
        async fn generate(
            &self,
            _req: GenerateRequest,
            _cancel: CancellationToken,
        ) -> std::result::Result<Completion, valyria_model::ModelError> {
            Err(valyria_model::ModelError::Unavailable {
                reason: "down".into(),
            })
        }
        fn stream(
            &self,
            _req: GenerateRequest,
            _cancel: CancellationToken,
        ) -> futures::stream::BoxStream<
            'static,
            std::result::Result<valyria_model::Chunk, valyria_model::ModelError>,
        > {
            futures::stream::empty().boxed()
        }
    }

    fn req() -> GenerateRequest {
        GenerateRequest::new(vec![Message::user("go")]).with_turn_hint(0)
    }

    #[tokio::test]
    async fn uses_the_primary_when_it_is_healthy() {
        let mut router = RoleRouter::new();
        router.register("primary", finishing_fake("from primary"));
        router.register("backup", finishing_fake("from backup"));
        router.bind(RoleBinding::new(Role::PrimaryCoder, "primary").with_fallback("backup"));

        let out = router
            .generate(Role::PrimaryCoder, req(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.model_id, "primary");
        assert_eq!(out.completion.text, "from primary");
    }

    #[tokio::test]
    async fn falls_back_when_the_primary_is_unavailable() {
        let mut router = RoleRouter::new();
        router.register("primary", Arc::new(DownRuntime));
        router.register("backup", finishing_fake("from backup"));
        router.bind(RoleBinding::new(Role::PrimaryCoder, "primary").with_fallback("backup"));

        let out = router
            .generate(Role::PrimaryCoder, req(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.model_id, "backup");
    }

    #[tokio::test]
    async fn missing_runtime_in_chain_is_skipped_not_fatal() {
        let mut router = RoleRouter::new();
        router.register("backup", finishing_fake("from backup"));
        router.bind(RoleBinding::new(Role::Planner, "primary").with_fallback("backup"));

        let out = router
            .generate(Role::Planner, req(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.model_id, "backup");
    }

    #[tokio::test]
    async fn exhausting_the_chain_reports_all_fallbacks_failed() {
        let mut router = RoleRouter::new();
        router.register("primary", Arc::new(DownRuntime));
        router.bind(RoleBinding::new(Role::PrimaryCoder, "primary"));

        let err = router
            .generate(Role::PrimaryCoder, req(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            OrchestratorError::AllFallbacksFailed {
                role: Role::PrimaryCoder,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn unbound_role_is_a_no_binding_error() {
        let router = RoleRouter::new();
        let err = router
            .generate(Role::Reviewer, req(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, OrchestratorError::NoBinding { .. }));
    }
}
