//! Workspace assertion inputs and validation shared by MCP read and admin routes.

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceInput {
    /// Workspace selected when the MCP server was constructed.
    pub(crate) workspace: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadWorkspaceInput {
    /// Optional assertion of the workspace already bound to this MCP server.
    #[serde(default)]
    pub(crate) workspace: Option<String>,
}

pub(crate) fn validate_workspace(configured: &str, requested: &str) -> Result<(), String> {
    if requested == configured {
        Ok(())
    } else {
        Err(format!(
            "workspace `{requested}` is outside this server's configured `{configured}` policy"
        ))
    }
}

pub(super) fn validate_optional_workspace(
    configured: &str,
    requested: Option<&str>,
) -> Result<(), String> {
    requested.map_or(Ok(()), |requested| {
        validate_workspace(configured, requested)
    })
}
