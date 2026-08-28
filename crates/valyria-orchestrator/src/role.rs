//! Model roles (§38). The canonical definition lives in
//! `valyria-model-registry`, next to the catalog that scores models for
//! each role; the orchestrator re-exports it as `Role` so existing callers
//! keep a stable path.

pub use valyria_model_registry::ModelRole as Role;
