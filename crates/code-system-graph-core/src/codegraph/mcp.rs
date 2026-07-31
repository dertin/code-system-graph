use std::ffi::OsString;
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, JsonObject
};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, ReadBuf};

use super::cli::{capture_bounded, command_for, join_capture, spawn_child, terminate};
use super::contract::{DiscoveredTool, map_mcp_tools};
use super::{invalid_response, output_limit, transport_error};
use crate::{
    LocalContextRequest, LocalContextResult, ProviderError, ProviderExecution, ProviderRequest, ProviderTransport
};

const PROTOCOL_OVERHEAD_BYTES: usize = 64 * 1024;
const STDERR_LIMIT_BYTES: usize = 16 * 1024;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) struct McpProbe {
    pub(crate) version: Option<String>,
    pub(crate) protocol_version: String,
    pub(crate) tools: Vec<DiscoveredTool>,
}

#[derive(Debug, Clone)]
pub(crate) struct CodeGraphMcp {
    binary: OsString,
}

type McpService = RunningService<RoleClient, ClientInfo>;

struct McpConnection {
    service: McpService,
    child: tokio::process::Child,
    stderr_task: Option<tokio::task::JoinHandle<std::io::Result<super::cli::CapturedOutput>>>,
    protocol_exceeded: Arc<AtomicBool>,
}

impl CodeGraphMcp {
    pub(crate) fn new(binary: OsString) -> Self {
        Self { binary }
    }

    pub(crate) async fn probe(&self, request: &ProviderRequest) -> Result<McpProbe, ProviderError> {
        let deadline = tokio::time::Instant::now() + request.budget.timeout;
        let mut connection = self.connect(request, deadline).await?;
        let tools = tokio::select! {
            biased;
            () = request.cancellation.cancelled() => {
                connection.shutdown().await;
                return Err(ProviderError::Cancelled);
            }
            () = tokio::time::sleep_until(deadline) => {
                connection.shutdown().await;
                return Err(super::timeout_error());
            }
            result = connection.service.list_all_tools() => {
                result.map_err(|error| {
                    invalid_response(format!("CodeGraph tools/list failed: {error}"))
                })?
            }
        };
        if connection.protocol_exceeded.load(Ordering::Relaxed) {
            connection.shutdown().await;
            return Err(output_limit(request.budget.max_output_bytes));
        }
        let peer_info = connection.service.peer_info();
        let tools = discovered_tools(tools);
        let probe = McpProbe {
            version: peer_info
                .as_ref()
                .and_then(|info| info.server_info.as_ref())
                .map(|info| info.version.clone()),
            protocol_version: peer_info.map_or_else(String::new, |info| {
                info.protocol_version.as_str().to_owned()
            }),
            tools,
        };
        connection.shutdown().await;
        Ok(probe)
    }

    pub(crate) async fn local_context(
        &self,
        input: &LocalContextRequest,
    ) -> Result<LocalContextResult, ProviderError> {
        let request = &input.request;
        let deadline = tokio::time::Instant::now() + request.budget.timeout;
        let mut connection = self.connect(request, deadline).await?;
        let tools = tokio::select! {
            biased;
            () = request.cancellation.cancelled() => {
                connection.shutdown().await;
                return Err(ProviderError::Cancelled);
            }
            () = tokio::time::sleep_until(deadline) => {
                connection.shutdown().await;
                return Err(super::timeout_error());
            }
            result = connection.service.list_all_tools() => {
                result.map_err(|error| {
                    invalid_response(format!("CodeGraph tools/list failed: {error}"))
                })?
            }
        };
        let discovered = discovered_tools(tools);
        let context_capability = map_mcp_tools(&discovered)
            .into_iter()
            .find(|capability| capability.operation == crate::ProviderOperation::LocalContext);
        let Some(context_capability) = context_capability else {
            connection.shutdown().await;
            return Err(invalid_response(
                "CodeGraph MCP exposes no compatible local-context tool",
            ));
        };
        let Some(tool) = discovered
            .iter()
            .find(|tool| tool.name == context_capability.public_name)
        else {
            connection.shutdown().await;
            return Err(invalid_response(
                "CodeGraph MCP context capability disappeared during discovery",
            ));
        };
        let arguments = context_arguments(input, tool);
        let params = CallToolRequestParams::new(tool.name.clone()).with_arguments(arguments);
        let result = tokio::select! {
            biased;
            () = request.cancellation.cancelled() => {
                connection.shutdown().await;
                return Err(ProviderError::Cancelled);
            }
            () = tokio::time::sleep_until(deadline) => {
                connection.shutdown().await;
                return Err(super::timeout_error());
            }
            result = connection.service.call_tool(params) => {
                result.map_err(|error| {
                    invalid_response(format!("CodeGraph tools/call failed: {error}"))
                })?
            }
        };
        if connection.protocol_exceeded.load(Ordering::Relaxed) {
            connection.shutdown().await;
            return Err(output_limit(request.budget.max_output_bytes));
        }
        if result.is_error == Some(true) {
            connection.shutdown().await;
            return Err(invalid_response(
                "CodeGraph context tool returned a tool-level error",
            ));
        }
        let text = result
            .content
            .iter()
            .filter_map(|content| content.as_text().map(|text| text.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() && result.structured_content.is_none() {
            connection.shutdown().await;
            return Err(invalid_response(
                "CodeGraph context tool returned no textual or structured content",
            ));
        }
        let (content, truncated) = truncate_utf8(text, request.budget.max_output_bytes);
        let output_bytes = content.len();
        connection.shutdown().await;
        Ok(LocalContextResult {
            content,
            execution: ProviderExecution {
                transport: ProviderTransport::Mcp,
                output_bytes,
                truncated,
                degradations: Vec::new(),
            },
        })
    }

    async fn connect(
        &self,
        request: &ProviderRequest,
        deadline: tokio::time::Instant,
    ) -> Result<McpConnection, ProviderError> {
        let mut command = command_for(&self.binary).map_err(|error| {
            transport_error(format!("cannot resolve CodeGraph executable: {error}"))
        })?;
        command
            .arg("serve")
            .arg("--mcp")
            .arg("--no-watch")
            .arg("--path")
            .arg(&request.project_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = spawn_child(&mut command)
            .await
            .map_err(|error| transport_error(format!("cannot start CodeGraph MCP: {error}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| transport_error("CodeGraph MCP stdin pipe was unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| transport_error("CodeGraph MCP stdout pipe was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| transport_error("CodeGraph MCP stderr pipe was unavailable"))?;
        let stderr_task = tokio::spawn(capture_bounded(stderr, STDERR_LIMIT_BYTES));
        let protocol_exceeded = Arc::new(AtomicBool::new(false));
        let protocol_limit = request
            .budget
            .max_output_bytes
            .saturating_add(PROTOCOL_OVERHEAD_BYTES);
        let reader = LimitedAsyncRead::new(stdout, protocol_limit, protocol_exceeded.clone());
        let client = ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("code-system-graph", env!("CARGO_PKG_VERSION")),
        );
        let service = tokio::select! {
            biased;
            () = request.cancellation.cancelled() => {
                terminate(&mut child).await;
                let _ = stderr_task.await;
                return Err(ProviderError::Cancelled);
            }
            () = tokio::time::sleep_until(deadline) => {
                terminate(&mut child).await;
                let _ = stderr_task.await;
                return Err(super::timeout_error());
            }
            result = client.serve((reader, stdin)) => {
                result.map_err(|error| {
                    if protocol_exceeded.load(Ordering::Relaxed) {
                        output_limit(request.budget.max_output_bytes)
                    } else {
                        invalid_response(format!("CodeGraph MCP initialize failed: {error}"))
                    }
                })?
            }
        };
        Ok(McpConnection {
            service,
            child,
            stderr_task: Some(stderr_task),
            protocol_exceeded,
        })
    }
}

impl McpConnection {
    async fn shutdown(&mut self) {
        let _ = self.service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(_) => {}
            Err(_) => terminate(&mut self.child).await,
        }
        if let Some(task) = self.stderr_task.take() {
            let _ = join_capture(task, "stderr").await;
        }
    }
}

fn context_arguments(input: &LocalContextRequest, tool: &DiscoveredTool) -> JsonObject {
    let properties = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object);
    let mut arguments = Map::new();
    arguments.insert("query".to_owned(), Value::String(input.query.clone()));
    if properties.is_some_and(|properties| properties.contains_key("maxFiles")) {
        arguments.insert(
            "maxFiles".to_owned(),
            Value::from(u64::try_from(input.max_files).unwrap_or(u64::MAX)),
        );
    }
    if properties.is_some_and(|properties| properties.contains_key("projectPath")) {
        arguments.insert(
            "projectPath".to_owned(),
            Value::String(input.request.project_path.to_string_lossy().into_owned()),
        );
    }
    arguments
}

fn discovered_tools(tools: Vec<rmcp::model::Tool>) -> Vec<DiscoveredTool> {
    tools
        .into_iter()
        .map(|tool| DiscoveredTool {
            name: tool.name.into_owned(),
            description: tool
                .description
                .map_or_else(String::new, std::borrow::Cow::into_owned),
            input_schema: Value::Object((*tool.input_schema).clone()),
        })
        .collect()
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    (value, true)
}

struct LimitedAsyncRead<R> {
    inner: R,
    remaining: usize,
    exceeded: Arc<AtomicBool>,
}

impl<R> LimitedAsyncRead<R> {
    fn new(inner: R, limit: usize, exceeded: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            remaining: limit,
            exceeded,
        }
    }
}

impl<R> AsyncRead for LimitedAsyncRead<R>
where
    R: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            this.exceeded.store(true, Ordering::Relaxed);
            return Poll::Ready(Err(io::Error::other("MCP protocol output limit exceeded")));
        }
        let requested = buffer.remaining().min(this.remaining).min(8192);
        if requested == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut temporary = [0_u8; 8192];
        let mut limited = ReadBuf::new(&mut temporary[..requested]);
        match Pin::new(&mut this.inner).poll_read(context, &mut limited) {
            Poll::Ready(Ok(())) => {
                let filled = limited.filled();
                this.remaining = this.remaining.saturating_sub(filled.len());
                buffer.put_slice(filled);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_utf8;

    #[test]
    fn truncation_should_preserve_utf8_boundaries() {
        let (value, truncated) = truncate_utf8("aéz".to_owned(), 2);

        assert_eq!(value, "a");
        assert!(truncated);
    }
}
