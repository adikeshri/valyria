//! Role routing (§38): bind a role to a model, delegate a generate call to
//! it. No pool, no fallback chain (D5's transport ladder lives one call up,
//! in [`crate::structured`]) — `model_role_binding` is one-model-per-role,
//! so a fallback chain would always be length 1; see `RoleRouter` for the
//! seam once that changes.
//!
//! Bindings are `&self`-mutable (`RwLock`, not `Arc`-frozen at
//! construction) so `model_activate` / `model_remove` can re-point a role
//! while the process runs — no daemon restart. The one invariant every
//! caller must keep: **never hold the read lock across a model call.** A
//! `generate` clones the `Arc<dyn ModelRuntime>` out under a lock held only
//! long enough for a `HashMap::get`, then drops the guard before
//! `.await`ing the model. Holding it longer would let a slow generation
//! (seconds to minutes, real inference) block `bind`/`rebind` — and every
//! *other* concurrent `generate` — for the same duration.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use valyria_model::{Completion, GenerateRequest, ModelRuntime};
use valyria_util::CancellationToken;

use crate::error::{OrchestratorError, Result};
use crate::role::Role;
use crate::structured;

#[derive(Default)]
pub struct Orchestrator {
    bindings: RwLock<HashMap<Role, Arc<dyn ModelRuntime>>>,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind (or replace) the runtime serving `role`. Safe to call while
    /// generations for `role` are in flight: an in-flight call already
    /// cloned its own handle and finishes against it; only the *next*
    /// call sees the new binding.
    pub fn bind(&self, role: Role, model: Arc<dyn ModelRuntime>) -> &Self {
        self.bindings
            .write()
            .expect("orchestrator bindings lock poisoned")
            .insert(role, model);
        self
    }

    /// Intent-revealing alias for [`Self::bind`] at a re-point call site
    /// (`model_activate` swapping an already-bound role).
    pub fn rebind(&self, role: Role, model: Arc<dyn ModelRuntime>) -> &Self {
        self.bind(role, model)
    }

    /// Drop the binding for `role` — the next `generate` for it errors
    /// `NoBinding` until something binds it again.
    pub fn clear(&self, role: Role) -> Option<Arc<dyn ModelRuntime>> {
        self.bindings
            .write()
            .expect("orchestrator bindings lock poisoned")
            .remove(&role)
    }

    pub fn is_bound(&self, role: Role) -> bool {
        self.bindings
            .read()
            .expect("orchestrator bindings lock poisoned")
            .contains_key(&role)
    }

    pub async fn generate(
        &self,
        role: Role,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<Completion> {
        let model = self.handle_for(role)?;
        let completion = model.generate(req, cancel).await?;
        Ok(completion)
    }

    /// [`Self::generate`], but run through the tool-call transport ladder
    /// (D5): a real model's messy output — fenced JSON, `[TOOL_CALLS]`
    /// tags, a stray second call — gets a bounded number of chances to be
    /// recovered or reformatted before the turn fails. Returns a
    /// normalized [`Completion`] so callers (`AgentDriver`'s journal,
    /// `ActionRequest::from_completion`) don't need to know the ladder
    /// ran at all: `ToolCalls` (never more than one — extra calls are
    /// dropped with a `warn!`, the driver is one-action-per-turn), `Ask`,
    /// or `Stop`.
    ///
    /// A well-behaved adapter (every `FakeModelRuntime` scenario) always
    /// returns a clean native shape and this costs exactly the one
    /// `generate` call `Self::generate` would have made.
    pub async fn generate_action(
        &self,
        role: Role,
        req: GenerateRequest,
        cancel: CancellationToken,
        max_reformat_retries: u32,
    ) -> Result<Completion> {
        let model = self.handle_for(role)?;
        let action =
            structured::resolve_action(model.as_ref(), req, &cancel, max_reformat_retries).await?;
        Ok(structured::action_to_completion(role, action))
    }

    /// Clone the `Arc` bound to `role` out from under a short-lived read
    /// guard. Never call this and hold onto the guard — the whole point
    /// is that the lock is gone before the caller does anything slow.
    fn handle_for(&self, role: Role) -> Result<Arc<dyn ModelRuntime>> {
        self.bindings
            .read()
            .expect("orchestrator bindings lock poisoned")
            .get(&role)
            .cloned()
            .ok_or(OrchestratorError::NoBinding { role })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_model::Message;
    use valyria_runtime_fake::{FakeModelRuntime, Scenario, ScriptedTurn};

    fn fake() -> Arc<dyn ModelRuntime> {
        Arc::new(FakeModelRuntime::from_scenario(Scenario {
            name: "t".into(),
            turns: vec![ScriptedTurn::Finish {
                summary: "done".into(),
            }],
        }))
    }

    #[tokio::test]
    async fn unbound_role_errors() {
        let orch = Orchestrator::new();
        let err = orch
            .generate(
                Role::PrimaryCoder,
                GenerateRequest::new(vec![Message::user("hi")]),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            OrchestratorError::NoBinding {
                role: Role::PrimaryCoder
            }
        ));
    }

    #[tokio::test]
    async fn bound_role_delegates_to_model() {
        let orch = Orchestrator::new();
        orch.bind(Role::PrimaryCoder, fake());
        let completion = orch
            .generate(
                Role::PrimaryCoder,
                GenerateRequest::new(vec![Message::user("hi")]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(completion.text, "done");
    }

    #[tokio::test]
    async fn rebind_takes_effect_on_the_next_generate() {
        let orch = Orchestrator::new();
        orch.bind(Role::PrimaryCoder, fake_with("a"));
        assert_eq!(
            orch.generate(Role::PrimaryCoder, req(), CancellationToken::new())
                .await
                .unwrap()
                .text,
            "a"
        );
        orch.rebind(Role::PrimaryCoder, fake_with("b"));
        assert_eq!(
            orch.generate(Role::PrimaryCoder, req(), CancellationToken::new())
                .await
                .unwrap()
                .text,
            "b"
        );
    }

    #[tokio::test]
    async fn clear_unbinds_the_role() {
        let orch = Orchestrator::new();
        orch.bind(Role::PrimaryCoder, fake());
        assert!(orch.is_bound(Role::PrimaryCoder));
        let removed = orch.clear(Role::PrimaryCoder);
        assert!(removed.is_some());
        assert!(!orch.is_bound(Role::PrimaryCoder));
        let err = orch
            .generate(Role::PrimaryCoder, req(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, OrchestratorError::NoBinding { .. }));
    }

    #[tokio::test]
    async fn rebind_during_an_in_flight_generate_does_not_affect_it() {
        // The whole point of cloning the Arc out before `.await`ing: a
        // generate call that already started keeps running against the
        // handle it was given, even if the role is rebound mid-flight.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let released = Arc::new(AtomicBool::new(false));
        let gate = released.clone();
        let slow = Arc::new(GatedRuntime { release: gate });
        let orch = Arc::new(Orchestrator::new());
        orch.bind(Role::PrimaryCoder, slow);

        let orch2 = orch.clone();
        let handle = tokio::spawn(async move {
            orch2
                .generate(Role::PrimaryCoder, req(), CancellationToken::new())
                .await
        });

        // Give the in-flight call a moment to start and observe the old
        // binding, then rebind before releasing it.
        tokio::time::sleep(Duration::from_millis(20)).await;
        orch.rebind(Role::PrimaryCoder, fake_with("new"));
        released.store(true, Ordering::SeqCst);

        let completion = handle.await.unwrap().unwrap();
        assert_eq!(completion.text, "old");
    }

    fn req() -> GenerateRequest {
        GenerateRequest::new(vec![Message::user("hi")])
    }

    fn fake_with(text: &str) -> Arc<dyn ModelRuntime> {
        Arc::new(FakeModelRuntime::from_scenario(Scenario {
            name: "t".into(),
            turns: vec![ScriptedTurn::Finish {
                summary: text.into(),
            }],
        }))
    }

    /// A `ModelRuntime` whose `generate` blocks until `release` flips true,
    /// then returns a fixed completion — used to prove a rebind mid-flight
    /// doesn't retroactively change an already-started call.
    struct GatedRuntime {
        release: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl ModelRuntime for GatedRuntime {
        fn capabilities(&self) -> valyria_model::Capabilities {
            valyria_model::Capabilities {
                context_length: 4096,
                supports_native_tools: true,
                supports_grammar: false,
                supports_streaming: false,
            }
        }
        async fn health(&self) -> valyria_model::Health {
            valyria_model::Health::Healthy
        }
        fn count_tokens(&self, text: &str) -> usize {
            text.len()
        }
        async fn generate(
            &self,
            _req: GenerateRequest,
            _cancel: CancellationToken,
        ) -> std::result::Result<Completion, valyria_model::ModelError> {
            while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            Ok(Completion {
                text: "old".into(),
                tool_calls: vec![],
                finish_reason: valyria_model::FinishReason::Stop,
                usage: valyria_model::TokenUsage::default(),
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
            unimplemented!("not exercised by this test")
        }
    }
}
