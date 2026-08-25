//! The `Tool` trait (§17).

use async_trait::async_trait;
use serde_json::Value;
use valyria_permissions::{Authorization, PermissionRequest};

use crate::ctx::ToolCtx;
use crate::descriptor::ToolDescriptor;
use crate::error::Result;
use crate::outcome::ToolOutcome;

#[async_trait]
pub trait Tool: Send + Sync {
    fn descriptor(&self) -> &ToolDescriptor;

    /// Map `input` to a permission request, without doing anything
    /// side-effecting. The runtime feeds this into the permission engine
    /// before ever calling `execute`.
    fn preflight(&self, ctx: &ToolCtx, input: &Value) -> Result<PermissionRequest>;

    /// Perform the tool's action. Implementations should call
    /// `auth.matches(...)` themselves before doing anything
    /// side-effecting — defense in depth, not just trust in the caller
    /// (see `valyria-permissions`'s `Authorization` docs, D2).
    async fn execute(&self, ctx: &ToolCtx, auth: &Authorization, input: Value) -> ToolOutcome;
}
