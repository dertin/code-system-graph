use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_hooks::templates::{
    AGENT_PLUGIN_GITIGNORE, AGENT_PLUGIN_LICENSE, AGENT_PLUGIN_MANIFEST, AGENT_PLUGIN_MCP, AGENT_PLUGIN_OPENAI_DEFAULT_PROMPT, AGENT_PLUGIN_OPENAI_DISPLAY_NAME, AGENT_PLUGIN_OPENAI_METADATA, AGENT_PLUGIN_OPERATING_GUIDE, AGENT_PLUGIN_SKILL, AGENT_PLUGIN_SKILL_DESCRIPTION
};
use code_system_graph_model::stable_id;

use super::{AgentPluginError, AgentPluginMcpBinding, BINDING_RELATIVE_PATH, pretty_json};

const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(super) fn render_files(
    workspace: &str,
    component_name: &str,
    binding: &AgentPluginMcpBinding,
) -> Result<BTreeMap<String, Vec<u8>>, AgentPluginError> {
    let plugin = render_json_template(
        "plugin.json",
        AGENT_PLUGIN_MANIFEST,
        &[
            ("PLUGIN_NAME", component_name.to_owned()),
            ("PLUGIN_VERSION", PLUGIN_VERSION.to_owned()),
            ("WORKSPACE_JSON_STRING", json_string_inner(workspace)?),
        ],
    )?;
    let mcp = render_json_template(
        "mcp.json",
        AGENT_PLUGIN_MCP,
        &[("MCP_SERVER_NAME", component_name.to_owned())],
    )?;
    let skill_files = render_skill_files(workspace, component_name, false)?;

    let mut files = BTreeMap::new();
    files.insert(
        ".gitignore".to_owned(),
        normalized_text_bytes(AGENT_PLUGIN_GITIGNORE),
    );
    files.insert(
        "LICENSE".to_owned(),
        normalized_text_bytes(AGENT_PLUGIN_LICENSE),
    );
    files.insert("mcp.json".to_owned(), mcp.into_bytes());
    files.insert("plugin.json".to_owned(), plugin.into_bytes());
    files.insert(
        BINDING_RELATIVE_PATH.to_owned(),
        pretty_json(
            "mcp-binding.json",
            &serde_json::to_value(binding).map_err(|source| AgentPluginError::Json {
                file: "mcp-binding.json",
                source,
            })?,
        )?,
    );
    files.extend(skill_files);
    Ok(files)
}

pub(super) fn render_existing_integration(
    workspace: &str,
    mcp_server_name: &str,
    skill_name: &str,
    include_openai_metadata: bool,
) -> Result<(serde_json::Value, BTreeMap<String, Vec<u8>>), AgentPluginError> {
    let mcp = render_json_template(
        "mcp.json",
        AGENT_PLUGIN_MCP,
        &[("MCP_SERVER_NAME", mcp_server_name.to_owned())],
    )?;
    let document: serde_json::Value =
        serde_json::from_str(&mcp).map_err(|source| AgentPluginError::Json {
            file: "mcp.json",
            source,
        })?;
    let server = document["mcpServers"][mcp_server_name].clone();
    Ok((
        server,
        render_skill_files(workspace, skill_name, include_openai_metadata)?,
    ))
}

fn render_skill_files(
    workspace: &str,
    skill_name: &str,
    include_openai_metadata: bool,
) -> Result<BTreeMap<String, Vec<u8>>, AgentPluginError> {
    let skill_description = render_template(
        "metadata/skill-description.txt",
        AGENT_PLUGIN_SKILL_DESCRIPTION,
        &[("WORKSPACE", workspace.to_owned())],
    );
    let skill_description = skill_description?.trim_end().to_owned();
    let skill = render_template(
        "skills/code-system-graph/SKILL.md",
        AGENT_PLUGIN_SKILL,
        &[
            ("SKILL_NAME", skill_name.to_owned()),
            ("SKILL_DESCRIPTION_YAML", json_value(&skill_description)?),
            ("WORKSPACE", workspace.to_owned()),
        ],
    )?;
    let operating_guide = render_template(
        "skills/code-system-graph/references/operating-guide.md",
        AGENT_PLUGIN_OPERATING_GUIDE,
        &[],
    )?;
    let skill_root = format!("skills/{skill_name}");
    let mut files = BTreeMap::from([
        (format!("{skill_root}/SKILL.md"), skill.into_bytes()),
        (
            format!("{skill_root}/references/operating-guide.md"),
            operating_guide.into_bytes(),
        ),
    ]);
    if include_openai_metadata {
        let display_name = render_template(
            "metadata/openai-display-name.txt",
            AGENT_PLUGIN_OPENAI_DISPLAY_NAME,
            &[("WORKSPACE", workspace.to_owned())],
        )?;
        let default_prompt = render_template(
            "metadata/openai-default-prompt.txt",
            AGENT_PLUGIN_OPENAI_DEFAULT_PROMPT,
            &[("SKILL_NAME", skill_name.to_owned())],
        )?;
        let openai_metadata = render_template(
            "skills/code-system-graph/agents/openai.yaml",
            AGENT_PLUGIN_OPENAI_METADATA,
            &[
                (
                    "SKILL_DISPLAY_NAME_YAML",
                    json_value(display_name.trim_end())?,
                ),
                (
                    "SKILL_DEFAULT_PROMPT_YAML",
                    json_value(default_prompt.trim_end())?,
                ),
            ],
        )?;
        files.insert(
            format!("{skill_root}/agents/openai.yaml"),
            openai_metadata.into_bytes(),
        );
    }
    Ok(files)
}

fn render_json_template(
    file: &'static str,
    template: &str,
    variables: &[(&'static str, String)],
) -> Result<String, AgentPluginError> {
    let rendered = render_template(file, template, variables)?;
    let _: serde_json::Value = serde_json::from_str(&rendered)
        .map_err(|source| AgentPluginError::Json { file, source })?;
    Ok(rendered)
}

fn render_template(
    file: &'static str,
    template: &str,
    variables: &[(&'static str, String)],
) -> Result<String, AgentPluginError> {
    let template = normalize_line_endings(template);
    let template = template.as_ref();
    let mut values = BTreeMap::new();
    for (name, value) in variables {
        if values.insert(*name, value.as_str()).is_some() {
            return Err(template_error(
                file,
                &format!("duplicate variable `{name}`"),
            ));
        }
    }

    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;
    let mut used = BTreeSet::new();
    while let Some(open) = remaining.find("{{") {
        rendered.push_str(&remaining[..open]);
        let placeholder = &remaining[open + 2..];
        let close = placeholder
            .find("}}")
            .ok_or_else(|| template_error(file, "unclosed variable"))?;
        let name = &placeholder[..close];
        let value = values
            .get(name)
            .ok_or_else(|| template_error(file, &format!("unknown variable `{name}`")))?;
        rendered.push_str(value);
        used.insert(name);
        remaining = &placeholder[close + 2..];
    }
    if remaining.contains("}}") {
        return Err(template_error(
            file,
            "closing delimiter without an opening delimiter",
        ));
    }
    rendered.push_str(remaining);
    let unused = values
        .keys()
        .copied()
        .filter(|name| !used.contains(name))
        .collect::<Vec<_>>();
    if !unused.is_empty() {
        return Err(template_error(
            file,
            &format!("unused variables: {}", unused.join(", ")),
        ));
    }
    Ok(rendered)
}

fn normalized_text_bytes(value: &str) -> Vec<u8> {
    normalize_line_endings(value).into_owned().into_bytes()
}

fn normalize_line_endings(value: &str) -> Cow<'_, str> {
    if value.contains('\r') {
        Cow::Owned(value.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(value)
    }
}

fn template_error(file: &'static str, detail: &str) -> AgentPluginError {
    AgentPluginError::Template {
        file,
        detail: detail.to_owned(),
    }
}

fn json_value(value: &str) -> Result<String, AgentPluginError> {
    serde_json::to_string(value).map_err(|source| AgentPluginError::Json {
        file: "template variable",
        source,
    })
}

fn json_string_inner(value: &str) -> Result<String, AgentPluginError> {
    let encoded = json_value(value)?;
    Ok(encoded[1..encoded.len() - 1].to_owned())
}

pub(super) fn workspace_component_name(workspace: &str) -> String {
    let mut slug = workspace
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        slug.push_str("workspace");
    }
    slug.truncate(30);
    slug = slug.trim_matches('-').to_owned();
    let identity = stable_id("agent-plugin-workspace", workspace);
    let digest = identity
        .rsplit_once(':')
        .map_or(identity.as_str(), |(_, digest)| digest);
    let suffix = digest.chars().take(10).collect::<String>();
    format!("code-system-graph-{slug}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::{normalize_line_endings, render_template};

    #[test]
    fn templates_should_render_with_portable_line_endings() {
        let rendered = render_template(
            "fixture.md",
            "---\r\nname: {{NAME}}\r\ndescription: portable\r---\r",
            &[("NAME", "example".to_owned())],
        )
        .expect("template should render");

        assert_eq!(rendered, "---\nname: example\ndescription: portable\n---\n");
        assert_eq!(
            normalize_line_endings("already\nportable\n"),
            "already\nportable\n"
        );
    }
}
