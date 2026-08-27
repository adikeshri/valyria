//! Minimal role routing (§38, cut down for Phase 3): bind a role to a
//! model, delegate a generate call to it. No pool, no admission control, no
//! transport ladder (D5) — those are Phase 9, once more than one adapter
//! and real models with unreliable tool-calling exist.

use std::collections::HashMap;
use std::sync::Arc;

use valyria_model::{Completion, GenerateRequest, ModelRuntime};
use valyria_util::CancellationToken;

use crate::error::{OrchestratorError, Result};
use crate::role::Role;

#[derive(Default)]
pub struct Orchestrator {
    bindings: HashMap<Role, Arc<dyn ModelRuntime>>,
}

impl Orchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, role: Role, model: Arc<dyn ModelRuntime>) -> &mut Self {
        self.bindings.insert(role, model);
        self
    }

    pub async fn generate(
        &self,
        role: Role,
        req: GenerateRequest,
        cancel: CancellationToken,
    ) -> Result<Completion> {
        let model = self
            .bindings
            .get(&role)
            .ok_or(OrchestratorError::NoBinding { role })?;
        let completion = model.generate(req, cancel).await?;
        Ok(completion)
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
        let mut orch = Orchestrator::new();
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
}
