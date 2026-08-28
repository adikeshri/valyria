//! [`ContextEngine`]: the whole §4.17 pipeline in one call.
//!
//! ```text
//! instructions  ─┐
//! memory        ─┼─► candidates ─► PromptAssembler ─► AssembledPrompt
//! retrieval     ─┤                 (rank → compress → budget → fence)
//! prior turns   ─┘
//! ```
//!
//! The engine's job is just the *conversion*: turn a discovered
//! [`InstructionSet`], a [`RetrievedMemory`], the output of a
//! [`Retriever`], and the recent turns into trust-tagged
//! [`RetrievalCandidate`]s, then hand them to [`PromptAssembler`], which
//! owns everything downstream (the trust lattice, the budget allocator,
//! nonce fencing, the replayable snapshot).

use valyria_instructions::InstructionSet;
use valyria_memory::RetrievedMemory;
use valyria_types::{Provenance, ProvenanceSource, Trust};

use crate::assemble::{AssembledPrompt, AssemblyRequest, PromptAssembler};
use crate::budget::{ContextBudget, SectionKind};
use crate::candidate::{CandidateContent, RetrievalCandidate};
use crate::error::Result;
use crate::retrieve::{RetrievalQuery, Retriever};

/// Everything the engine needs for one assembly.
#[derive(Debug, Clone, Default)]
pub struct EngineInput {
    pub task_intent: String,
    pub budget_tokens: usize,
    /// Override the default budget shape. `None` uses
    /// [`ContextBudget::new`].
    pub budget: Option<ContextBudget>,
    pub query: RetrievalQuery,
    pub instructions: Option<InstructionSet>,
    pub memory: Option<RetrievedMemory>,
    /// Recent model turns, oldest first.
    pub prior_turns: Vec<String>,
}

impl EngineInput {
    pub fn new(task_intent: impl Into<String>, budget_tokens: usize) -> Self {
        let task_intent = task_intent.into();
        Self {
            query: RetrievalQuery::new(task_intent.clone()),
            task_intent,
            budget_tokens,
            ..Default::default()
        }
    }

    pub fn with_instructions(mut self, set: InstructionSet) -> Self {
        self.instructions = Some(set);
        self
    }

    pub fn with_memory(mut self, memory: RetrievedMemory) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_query(mut self, query: RetrievalQuery) -> Self {
        self.query = query;
        self
    }

    fn budget(&self) -> ContextBudget {
        self.budget
            .clone()
            .unwrap_or_else(|| ContextBudget::new(self.budget_tokens))
    }
}

pub struct ContextEngine<R: Retriever> {
    retriever: R,
    assembler: PromptAssembler,
}

impl<R: Retriever + std::fmt::Debug> std::fmt::Debug for ContextEngine<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextEngine")
            .field("retriever", &self.retriever)
            .field("assembler", &self.assembler)
            .finish()
    }
}

impl<R: Retriever> ContextEngine<R> {
    pub fn new(retriever: R) -> Self {
        Self {
            retriever,
            assembler: PromptAssembler::new(),
        }
    }

    pub fn with_assembler(mut self, assembler: PromptAssembler) -> Self {
        self.assembler = assembler;
        self
    }

    pub fn retriever(&self) -> &R {
        &self.retriever
    }

    /// Run the pipeline. Retrieval errors and an infeasible budget both
    /// propagate — the caller narrows the task rather than silently
    /// shipping a truncated prompt.
    pub async fn build(&self, input: EngineInput) -> Result<AssembledPrompt> {
        let mut candidates: Vec<RetrievalCandidate> = Vec::new();

        if let Some(set) = &input.instructions {
            candidates.extend(instruction_candidates(set));
        }
        if let Some(mem) = &input.memory {
            candidates.extend(memory_candidates(mem));
        }
        candidates.extend(self.retriever.retrieve(&input.query).await?);
        candidates.extend(history_candidates(&input.prior_turns));

        let req = AssemblyRequest::new(input.task_intent.clone(), input.budget())
            .with_candidates(candidates);
        self.assembler.assemble(req)
    }
}

fn instruction_candidates(set: &InstructionSet) -> Vec<RetrievalCandidate> {
    let mut out = Vec::new();
    for (i, source) in set.sources.iter().enumerate() {
        // Highest-authority sources are the most relevant; advisory files
        // sit near the bottom.
        let relevance = if source.is_directive() {
            (1.0 - i as f64 * 0.05).max(0.5)
        } else {
            0.35
        };
        let prov = Provenance::new(ProvenanceSource::Instruction {
            path: source.origin.display().to_string(),
        })
        .with_step("instructions:discover")
        .with_step(format!("authority:{}", source.authority.label()));
        out.push(RetrievalCandidate::new(
            source.trust,
            prov,
            SectionKind::Instructions,
            relevance,
            CandidateContent::text(source.body.clone()),
        ));
    }

    if !set.conflicts.is_empty() {
        let body = set
            .conflicts
            .iter()
            .map(|c| {
                format!(
                    "- \"{}\" ({}) overrides \"{}\" ({})",
                    c.winner_line,
                    c.winner.display(),
                    c.loser_line,
                    c.loser.display()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.push(RetrievalCandidate::new(
            Trust::Evidence,
            Provenance::new(ProvenanceSource::ModelTurn).with_step("instructions:conflicts"),
            SectionKind::Instructions,
            0.4,
            CandidateContent::text(format!(
                "Instruction conflicts detected (higher authority wins):\n{body}"
            )),
        ));
    }

    out
}

fn memory_candidates(mem: &RetrievedMemory) -> Vec<RetrievalCandidate> {
    let mut out = Vec::new();
    for entry in &mem.pinned {
        out.push(RetrievalCandidate::new(
            entry.trust(),
            Provenance::new(ProvenanceSource::Memory { id: entry.id }).with_step("memory:pinned"),
            SectionKind::Memory,
            0.85,
            CandidateContent::text(entry.text.clone()),
        ));
    }
    for scored in &mem.ranked {
        out.push(RetrievalCandidate::new(
            scored.entry.trust(),
            Provenance::new(ProvenanceSource::Memory {
                id: scored.entry.id,
            })
            .with_step("memory:ranked")
            .with_score(scored.score),
            SectionKind::Memory,
            scored.score.clamp(0.0, 1.0),
            CandidateContent::text(scored.entry.text.clone()),
        ));
    }
    out
}

fn history_candidates(turns: &[String]) -> Vec<RetrievalCandidate> {
    let n = turns.len();
    turns
        .iter()
        .enumerate()
        .map(|(i, turn)| {
            // More recent turns are more relevant.
            let relevance = (i + 1) as f64 / n as f64;
            RetrievalCandidate::new(
                Trust::ModelOutput,
                Provenance::new(ProvenanceSource::ModelTurn).with_step("history:prior_turn"),
                SectionKind::History,
                relevance,
                CandidateContent::text(turn.clone()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_instructions::Discovery;
    use valyria_memory::{MemoryEntry, MemoryKind, MemoryScope, RetrievedMemory, ScoredMemory};

    use crate::retrieve::StaticRetriever;

    fn engine() -> ContextEngine<StaticRetriever> {
        ContextEngine::new(StaticRetriever::empty()).with_assembler(
            PromptAssembler::new()
                .with_policy("POLICY")
                .with_rng(std::sync::Arc::new(
                    valyria_util::DeterministicRng::from_seed(3),
                )),
        )
    }

    #[tokio::test]
    async fn instructions_and_memory_flow_into_the_right_sections() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "always run the tests").unwrap();
        let set = Discovery::new(dir.path()).discover().unwrap();

        let mem = RetrievedMemory {
            pinned: vec![],
            ranked: vec![ScoredMemory {
                entry: MemoryEntry::agent(
                    MemoryScope::Repository,
                    MemoryKind::Command,
                    "cargo nextest run is the test command",
                    "task_1",
                    0.9,
                    0,
                ),
                relevance: 0.8,
                effective_confidence: 0.9,
                score: 0.72,
            }],
        };

        let input = EngineInput::new("make the tests faster", 5_000)
            .with_instructions(set)
            .with_memory(mem);
        let out = engine().build(input).await.unwrap();

        // Instruction (directive) -> system message.
        assert!(out.messages[0].content.contains("always run the tests"));
        // Agent memory is Evidence -> fenced data block.
        assert!(out
            .messages
            .last()
            .unwrap()
            .content
            .contains("cargo nextest run"));
    }

    #[tokio::test]
    async fn prior_turns_become_history_candidates() {
        let mut input = EngineInput::new("continue", 5_000);
        input.prior_turns = vec!["I read src/main.rs".into(), "I edited the parser".into()];
        let out = engine().build(input).await.unwrap();
        assert!(out
            .snapshot
            .items
            .iter()
            .any(|i| i.section == SectionKind::History));
    }

    #[tokio::test]
    async fn retrieval_error_propagates() {
        struct Failing;
        #[async_trait::async_trait]
        impl Retriever for Failing {
            async fn retrieve(&self, _q: &RetrievalQuery) -> Result<Vec<RetrievalCandidate>> {
                Err(crate::ContextError::Retrieval("boom".into()))
            }
        }
        let eng = ContextEngine::new(Failing);
        let err = eng.build(EngineInput::new("x", 5_000)).await.unwrap_err();
        assert!(matches!(err, crate::ContextError::Retrieval(_)));
    }
}
