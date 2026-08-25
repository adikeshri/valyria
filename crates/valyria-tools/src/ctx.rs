//! What every tool gets to work with. Deliberately narrow: a tool cannot
//! reach the permission engine, the model, or anything outside the
//! workspace root through this — it only gets what it needs to read/write
//! files, run processes, and record what it did.

use std::sync::Arc;

use valyria_types::{StepId, TaskId};
use valyria_util::CancellationToken;
use valyria_vfs::{HashCache, WorkspaceRoot};

use valyria_ledger::Ledger;

#[derive(Clone)]
pub struct ToolCtx {
    pub workspace_root: WorkspaceRoot,
    pub hash_cache: Arc<HashCache>,
    pub ledger: Arc<Ledger>,
    pub task_id: TaskId,
    pub step_id: StepId,
    pub cancel: CancellationToken,
}
