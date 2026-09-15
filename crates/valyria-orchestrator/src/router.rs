//! Role routing with fallback chains (§38). A [`RoleBinding`] names a
//! primary model and an ordered list of fallbacks; [`RoleRouter::generate`]
//! and [`RoleRouter::generate_action`] walk that chain, skipping a model
//! that has no registered runtime or reports itself unavailable, and
//! retrying the next one on a *retryable* model error (or, for
//! `generate_action`, a ladder that exhausted its reformat retries without
//! ever producing a parseable turn — the same "this model is unreliable,
//! not this request is bad" reasoning, just discovered one layer up). A
//! non-retryable model error is surfaced immediately.
//!
//! Bindings and registered runtimes live behind an `RwLock` (mirroring
//! [`crate::Orchestrator`]) so `model_activate` can re-point a role, add a
//! fallback, or register a newly-installed model's runtime while a task is
//! mid-flight — no daemon restart. The same invariant applies: never hold
//! the read lock across a model call. Each loop iteration clones the
//! `Arc<dyn ModelRuntime>` it needs out from under a lock held only long
//! enough for the two `HashMap` lookups, then drops the guard before
//! `.await`ing the model.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use valyria_model::{Completion, GenerateRequest, Health, ModelRuntime};
use valyria_types::ErrorCode;
use valyria_util::CancellationToken;

use crate::error::{OrchestratorError, Result};
use crate::role::Role;
use crate::structured;

pub use valyria_model_registry::RoleBinding;

/// A completion plus which model in the chain actually produced it — the
/// caller records this so "which model did this work?" is answerable.
#[derive(Debug, Clone)]
pub struct RoutedCompletion {
    pub model_id: String,
    pub completion: Completion,
}

#[derive(Default)]
struct RouterState {
    runtimes: HashMap<String, Arc<dyn ModelRuntime>>,
    bindings: HashMap<Role, RoleBinding>,
}

#[derive(Default)]
pub struct RoleRouter {
    state: RwLock<RouterState>,
}

impl std::fmt::Debug for RoleRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.state.read().expect("router state lock poisoned");
        f.debug_struct("RoleRouter")
            .field("runtimes", &s.runtimes.keys().collect::<Vec<_>>())
            .field("bindings", &s.bindings.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl RoleRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make a concrete model runtime available under its catalog id.
    /// Safe to call while generations are in flight (see the module docs).
    pub fn register(&self, model_id: impl Into<String>, runtime: Arc<dyn ModelRuntime>) -> &Self {
        self.state
            .write()
            .expect("router state lock poisoned")
            .runtimes
            .insert(model_id.into(), runtime);
        self
    }

    /// Drop a registered runtime — e.g. `model_remove` after every role
    /// bound to it has been repointed elsewhere.
    pub fn unregister(&self, model_id: &str) -> Option<Arc<dyn ModelRuntime>> {
        self.state
            .write()
            .expect("router state lock poisoned")
            .runtimes
            .remove(model_id)
    }

    /// Set (or replace) `role`'s fallback chain. Intent-revealing alias:
    /// `rebind` at a re-point call site (`model_activate` repointing an
    /// already-bound role).
    pub fn bind(&self, binding: RoleBinding) -> &Self {
        self.state
            .write()
            .expect("router state lock poisoned")
            .bindings
            .insert(binding.role, binding);
        self
    }

    pub fn rebind(&self, binding: RoleBinding) -> &Self {
        self.bind(binding)
    }

    /// Convenience for the common case: register `runtime` under `model_id`
    /// and bind `role` to it as a length-1 chain in one call — the same
    /// ergonomics as [`crate::Orchestrator::bind`], for call sites (tests,
    /// a manual single-model setup) that don't need a fallback chain.
    /// A later `bind(RoleBinding::new(role, model_id).with_fallback(..))`
    /// on the same router adds fallbacks without re-registering.
    pub fn bind_single(
        &self,
        role: Role,
        model_id: impl Into<String>,
        runtime: Arc<dyn ModelRuntime>,
    ) -> &Self {
        let id = model_id.into();
        self.register(id.clone(), runtime);
        self.bind(RoleBinding::new(role, id));
        self
    }

    /// Drop `role`'s binding — the next call for it errors `NoBinding`
    /// until something binds it again. The registered runtime (if nothing
    /// else references it) is left registered; `unregister` drops that too.
    pub fn clear(&self, role: Role) -> Option<RoleBinding> {
        self.state
            .write()
            .expect("router state lock poisoned")
            .bindings
            .remove(&role)
    }

    pub fn is_bound(&self, role: Role) -> bool {
        self.state
            .read()
            .expect("router state lock poisoned")
            .bindings
            .contains_key(&role)
    }

    pub fn binding(&self, role: Role) -> Option<RoleBinding> {
        self.state
            .read()
            .expect("router state lock poisoned")
            .bindings
            .get(&role)
            .cloned()
    }

    fn runtime_for(&self, id: &str) -> Option<Arc<dyn ModelRuntime>> {
        self.state
            .read()
            .expect("router state lock poisoned")
            .runtimes
            .get(id)
            .cloned()
    }

    pub async fn generate(
        &self,
        role: Role,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<RoutedCompletion> {
        let binding = self
            .binding(role)
            .ok_or(OrchestratorError::NoBinding { role })?;

        let mut last = "no candidate model was reachable".to_string();
        for id in binding.chain() {
            let Some(runtime) = self.runtime_for(id) else {
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

    /// [`Self::generate`]'s sibling for the live agent loop: for each
    /// candidate in `role`'s fallback chain (in order), run the full D5
    /// transport ladder ([`structured::resolve_action`]) against it rather
    /// than a single bare `generate`. A candidate is skipped — exactly as in
    /// `generate` — when unregistered, unhealthy, or its ladder attempt
    /// fails with a retryable model error *or* exhausts its reformat
    /// retries without ever recovering a parseable turn
    /// (`UnparseableToolCall`): a model that can't format tool calls
    /// reliably after `max_reformat_retries` tries is exactly the case a
    /// fallback chain exists for, same as a model that's down. Any other
    /// error is fatal immediately — a fallback chain recovers from "this
    /// model is unreliable", not "this request is malformed".
    pub async fn generate_action(
        &self,
        role: Role,
        req: GenerateRequest,
        cancel: CancellationToken,
        max_reformat_retries: u32,
    ) -> Result<RoutedCompletion> {
        let binding = self
            .binding(role)
            .ok_or(OrchestratorError::NoBinding { role })?;

        let mut last = "no candidate model was reachable".to_string();
        for id in binding.chain() {
            let Some(runtime) = self.runtime_for(id) else {
                last = format!("no runtime registered for {id:?}");
                continue;
            };
            if let Health::Unavailable { reason } = runtime.health().await {
                last = format!("{id:?} unavailable: {reason}");
                continue;
            }
            match structured::resolve_action(
                runtime.as_ref(),
                req.clone(),
                &cancel,
                max_reformat_retries,
            )
            .await
            {
                Ok(action) => {
                    return Ok(RoutedCompletion {
                        model_id: id.to_string(),
                        completion: structured::action_to_completion(role, action),
                    })
                }
                Err(e @ OrchestratorError::Model(_)) if e.retryable() => {
                    tracing::warn!(model = %id, error = %e, "falling back to next model in chain");
                    last = e.to_string();
                    continue;
                }
                Err(e @ OrchestratorError::UnparseableToolCall { .. }) => {
                    tracing::warn!(
                        model = %id,
                        error = %e,
                        "model exhausted its reformat retries; falling back to next model in chain"
                    );
                    last = e.to_string();
                    continue;
                }
                Err(e) => return Err(e),
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
        let router = RoleRouter::new();
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
        let router = RoleRouter::new();
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
        let router = RoleRouter::new();
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
        let router = RoleRouter::new();
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

    // --- generate_action (the ladder, walked across the fallback chain) --

    #[tokio::test]
    async fn generate_action_uses_the_primary_when_healthy() {
        let router = RoleRouter::new();
        router.bind_single(Role::PrimaryCoder, "primary", finishing_fake("done"));

        let out = router
            .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
            .await
            .unwrap();
        assert_eq!(out.model_id, "primary");
        assert_eq!(out.completion.text, "done");
    }

    #[tokio::test]
    async fn generate_action_falls_back_when_the_primary_is_down() {
        let router = RoleRouter::new();
        router.register("primary", Arc::new(DownRuntime));
        router.register("backup", finishing_fake("from backup"));
        router.bind(RoleBinding::new(Role::PrimaryCoder, "primary").with_fallback("backup"));

        let out = router
            .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
            .await
            .unwrap();
        assert_eq!(out.model_id, "backup");
    }

    /// The case D5 + §38 exist to cover together: a model that answers but
    /// can never format a parseable tool call is not "down" in the health
    /// sense `DownRuntime` models — it looks perfectly healthy — but it is
    /// exactly as unreliable, and the router falls back to the next model
    /// in the chain rather than surfacing `UnparseableToolCall` to the
    /// driver.
    #[tokio::test]
    async fn generate_action_falls_back_when_the_primary_cannot_format_a_tool_call() {
        let bad = ScriptedTurn::Malformed {
            raw: "<tool_call>{ not json }</tool_call>".into(),
        };
        let router = RoleRouter::new();
        router.register(
            "unreliable",
            Arc::new(FakeModelRuntime::from_scenario(scenario_of(vec![
                bad.clone(),
                bad.clone(),
                bad,
            ]))),
        );
        router.register("backup", finishing_fake("from backup"));
        router.bind(RoleBinding::new(Role::PrimaryCoder, "unreliable").with_fallback("backup"));

        let out = router
            .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 2)
            .await
            .unwrap();
        assert_eq!(out.model_id, "backup");
        assert_eq!(out.completion.text, "from backup");
    }

    #[tokio::test]
    async fn generate_action_exhausting_the_chain_reports_all_fallbacks_failed() {
        let bad = ScriptedTurn::Malformed {
            raw: "<tool_call>{ not json }</tool_call>".into(),
        };
        let router = RoleRouter::new();
        router.register(
            "only",
            Arc::new(FakeModelRuntime::from_scenario(scenario_of(vec![
                bad.clone(),
                bad,
            ]))),
        );
        router.bind(RoleBinding::new(Role::PrimaryCoder, "only"));

        let err = router
            .generate_action(Role::PrimaryCoder, req(), CancellationToken::new(), 1)
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

    fn scenario_of(turns: Vec<ScriptedTurn>) -> Scenario {
        Scenario {
            name: "generate_action_router".into(),
            turns,
        }
    }
}
