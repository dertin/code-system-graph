use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Stdio;

use rmcp::transport::which_command;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::contract::{
    AffectedTestsContract, ImpactContract, NeighborsContract, StatusContract, SymbolQueryContract
};
use super::{diagnostic_id, invalid_response, output_limit, transport_error};
use crate::{
    AffectedTestsRequest, AffectedTestsResult, LocalContextRequest, LocalContextResult, LocalImpactRequest, LocalImpactResult, LocalNeighborDirection, LocalNeighborResult, LocalNeighborsRequest, ProviderError, ProviderExecution, ProviderRequest, ProviderTransport, ResolveSymbolsRequest, ResolveSymbolsResult
};

const STDERR_LIMIT_BYTES: usize = 16 * 1024;
const TEXT_FILE_BUSY_OS_ERROR: i32 = 26;

#[derive(Debug, Clone)]
pub(crate) struct CodeGraphCli {
    binary: OsString,
}

#[derive(Debug)]
struct CommandOutput {
    stdout: Vec<u8>,
    stdout_exceeded: bool,
}

#[derive(Debug)]
pub(super) struct CapturedOutput {
    pub(super) bytes: Vec<u8>,
    pub(super) exceeded: bool,
}

impl CodeGraphCli {
    pub(crate) fn new(binary: OsString) -> Self {
        Self { binary }
    }

    pub(crate) async fn version(&self, request: &ProviderRequest) -> Result<String, ProviderError> {
        let output = self
            .run(
                [OsString::from("--version")],
                request,
                request.budget.max_output_bytes,
            )
            .await?;
        if output.stdout_exceeded {
            return Err(output_limit(request.budget.max_output_bytes));
        }
        bounded_utf8(output.stdout, "CodeGraph version").map(|version| version.trim().to_owned())
    }

    pub(crate) async fn status(
        &self,
        request: &ProviderRequest,
    ) -> Result<StatusContract, ProviderError> {
        self.run_json(
            [
                OsString::from("status"),
                OsString::from("--json"),
                request.project_path.as_os_str().to_owned(),
            ],
            request,
        )
        .await
    }

    pub(crate) async fn resolve_symbols(
        &self,
        input: &ResolveSymbolsRequest,
    ) -> Result<ResolveSymbolsResult, ProviderError> {
        let request = &input.request;
        let limit = request.budget.max_items.to_string();
        let contracts: Vec<SymbolQueryContract> = self
            .run_json(
                [
                    OsString::from("query"),
                    OsString::from("--json"),
                    OsString::from("--limit"),
                    OsString::from(limit),
                    OsString::from("--path"),
                    request.project_path.as_os_str().to_owned(),
                    OsString::from("--"),
                    OsString::from(&input.query),
                ],
                request,
            )
            .await?;
        let provider_count = contracts.len();
        let symbols = contracts
            .into_iter()
            .take(request.budget.max_items)
            .map(SymbolQueryContract::into_symbol)
            .collect::<Vec<_>>();
        let output_bytes = serialized_size(&symbols)?;
        Ok(ResolveSymbolsResult {
            symbols,
            execution: ProviderExecution {
                transport: ProviderTransport::Cli,
                output_bytes,
                truncated: provider_count > request.budget.max_items,
                degradations: Vec::new(),
            },
        })
    }

    pub(crate) async fn local_neighbors(
        &self,
        input: &LocalNeighborsRequest,
    ) -> Result<LocalNeighborResult, ProviderError> {
        let request = &input.request;
        let command = match input.direction {
            LocalNeighborDirection::Callers => "callers",
            LocalNeighborDirection::Callees => "callees",
        };
        let contract: NeighborsContract = self
            .run_json(
                [
                    OsString::from(command),
                    OsString::from("--json"),
                    OsString::from("--limit"),
                    OsString::from(request.budget.max_items.to_string()),
                    OsString::from("--path"),
                    request.project_path.as_os_str().to_owned(),
                    OsString::from("--"),
                    OsString::from(&input.symbol),
                ],
                request,
            )
            .await?;
        let raw_neighbors = match input.direction {
            LocalNeighborDirection::Callers => contract.callers,
            LocalNeighborDirection::Callees => contract.callees,
        };
        let provider_count = raw_neighbors.len();
        let neighbors = raw_neighbors
            .into_iter()
            .take(request.budget.max_items)
            .map(super::contract::NeighborContract::into_neighbor)
            .collect::<Vec<_>>();
        let output_bytes = serialized_size(&neighbors)?;
        Ok(LocalNeighborResult {
            symbol: contract.symbol,
            direction: input.direction,
            neighbors,
            execution: ProviderExecution {
                transport: ProviderTransport::Cli,
                output_bytes,
                truncated: provider_count > request.budget.max_items,
                degradations: Vec::new(),
            },
        })
    }

    pub(crate) async fn local_impact(
        &self,
        input: &LocalImpactRequest,
    ) -> Result<LocalImpactResult, ProviderError> {
        let request = &input.request;
        let contract: ImpactContract = self
            .run_json(
                [
                    OsString::from("impact"),
                    OsString::from("--json"),
                    OsString::from("--depth"),
                    OsString::from(input.max_depth.to_string()),
                    OsString::from("--path"),
                    request.project_path.as_os_str().to_owned(),
                    OsString::from("--"),
                    OsString::from(&input.symbol),
                ],
                request,
            )
            .await?;
        let provider_count = contract.affected.len();
        let affected = contract
            .affected
            .into_iter()
            .take(request.budget.max_items)
            .map(super::contract::NeighborContract::into_neighbor)
            .collect::<Vec<_>>();
        let output_bytes = serialized_size(&affected)?;
        Ok(LocalImpactResult {
            symbol: contract.symbol,
            depth: contract.depth,
            provider_node_count: contract.node_count,
            affected,
            execution: ProviderExecution {
                transport: ProviderTransport::Cli,
                output_bytes,
                truncated: provider_count > request.budget.max_items,
                degradations: Vec::new(),
            },
        })
    }

    pub(crate) async fn local_context(
        &self,
        input: &LocalContextRequest,
    ) -> Result<LocalContextResult, ProviderError> {
        let request = &input.request;
        let output = self
            .run(
                [
                    OsString::from("explore"),
                    OsString::from("--max-files"),
                    OsString::from(input.max_files.to_string()),
                    OsString::from("--path"),
                    request.project_path.as_os_str().to_owned(),
                    OsString::from("--"),
                    OsString::from(&input.query),
                ],
                request,
                request.budget.max_output_bytes,
            )
            .await?;
        let content = if output.stdout_exceeded {
            String::from_utf8_lossy(&output.stdout).into_owned()
        } else {
            bounded_utf8(output.stdout, "CodeGraph context")?
        };
        Ok(LocalContextResult {
            execution: ProviderExecution {
                transport: ProviderTransport::Cli,
                output_bytes: content.len(),
                truncated: output.stdout_exceeded,
                degradations: Vec::new(),
            },
            content,
        })
    }

    pub(crate) async fn affected_tests(
        &self,
        input: &AffectedTestsRequest,
    ) -> Result<AffectedTestsResult, ProviderError> {
        let request = &input.request;
        let mut arguments = vec![
            OsString::from("affected"),
            OsString::from("--json"),
            OsString::from("--depth"),
            OsString::from(input.max_depth.to_string()),
            OsString::from("--path"),
            request.project_path.as_os_str().to_owned(),
            OsString::from("--"),
        ];
        arguments.extend(input.changed_files.iter().map(OsString::from));
        let contract: AffectedTestsContract = self.run_json(arguments, request).await?;
        let provider_count = contract.affected_tests.len();
        let affected_tests = contract
            .affected_tests
            .into_iter()
            .take(request.budget.max_items)
            .collect::<Vec<_>>();
        let output_bytes = serialized_size(&affected_tests)?;
        Ok(AffectedTestsResult {
            changed_files: contract.changed_files,
            affected_tests,
            total_dependents_traversed: contract.total_dependents_traversed,
            execution: ProviderExecution {
                transport: ProviderTransport::Cli,
                output_bytes,
                truncated: provider_count > request.budget.max_items,
                degradations: Vec::new(),
            },
        })
    }

    async fn run_json<T>(
        &self,
        arguments: impl IntoIterator<Item = OsString>,
        request: &ProviderRequest,
    ) -> Result<T, ProviderError>
    where
        T: DeserializeOwned,
    {
        let output = self
            .run(arguments, request, request.budget.max_output_bytes)
            .await?;
        if output.stdout_exceeded {
            return Err(output_limit(request.budget.max_output_bytes));
        }
        serde_json::from_slice(&output.stdout).map_err(|error| {
            invalid_response(format!(
                "CodeGraph CLI JSON did not match its contract: {error}"
            ))
        })
    }

    async fn run(
        &self,
        arguments: impl IntoIterator<Item = OsString>,
        request: &ProviderRequest,
        stdout_limit: usize,
    ) -> Result<CommandOutput, ProviderError> {
        let mut command = command_for(&self.binary).map_err(|error| {
            transport_error(format!("cannot resolve CodeGraph executable: {error}"))
        })?;
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = spawn_child(&mut command)
            .await
            .map_err(|error| transport_error(format!("cannot start CodeGraph: {error}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| transport_error("CodeGraph stdout pipe was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| transport_error("CodeGraph stderr pipe was unavailable"))?;
        let stdout_task = tokio::spawn(capture_bounded(stdout, stdout_limit));
        let stderr_task = tokio::spawn(capture_bounded(stderr, STDERR_LIMIT_BYTES));
        let deadline = tokio::time::Instant::now() + request.budget.timeout;
        let status = tokio::select! {
            biased;
            () = request.cancellation.cancelled() => {
                terminate(&mut child).await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(ProviderError::Cancelled);
            }
            () = tokio::time::sleep_until(deadline) => {
                terminate(&mut child).await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(ProviderError::Timeout {
                    diagnostics_id: diagnostic_id(),
                });
            }
            status = child.wait() => status.map_err(|error| {
                transport_error(format!("cannot wait for CodeGraph: {error}"))
            })?,
        };
        let stdout = join_capture(stdout_task, "stdout").await?;
        let stderr = join_capture(stderr_task, "stderr").await?;
        if !status.success() {
            let code = status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string());
            let message = String::from_utf8_lossy(&stderr.bytes);
            return Err(transport_error(format!(
                "CodeGraph exited with {code}: {}",
                message.trim()
            )));
        }
        Ok(CommandOutput {
            stdout: stdout.bytes,
            stdout_exceeded: stdout.exceeded,
        })
    }
}

pub(super) fn command_for(binary: &OsStr) -> std::io::Result<Command> {
    let path = Path::new(binary);
    if path.components().count() > 1 || path.is_absolute() {
        Ok(Command::new(binary))
    } else {
        which_command(binary)
    }
}

pub(super) async fn spawn_child(command: &mut Command) -> std::io::Result<tokio::process::Child> {
    let mut retries = 0_u8;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            // A just-replaced executable can remain transiently busy on Unix filesystems.
            Err(error) if error.raw_os_error() == Some(TEXT_FILE_BUSY_OS_ERROR) && retries < 3 => {
                retries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) async fn capture_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<CapturedOutput> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        exceeded |= retained < count;
    }
    Ok(CapturedOutput { bytes, exceeded })
}

pub(super) async fn join_capture(
    task: tokio::task::JoinHandle<std::io::Result<CapturedOutput>>,
    stream: &str,
) -> Result<CapturedOutput, ProviderError> {
    task.await
        .map_err(|error| transport_error(format!("CodeGraph {stream} reader failed: {error}")))?
        .map_err(|error| transport_error(format!("cannot read CodeGraph {stream}: {error}")))
}

pub(super) async fn terminate(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn bounded_utf8(bytes: Vec<u8>, context: &str) -> Result<String, ProviderError> {
    String::from_utf8(bytes)
        .map_err(|error| invalid_response(format!("{context} was not valid UTF-8: {error}")))
}

fn serialized_size<T: serde::Serialize>(value: &T) -> Result<usize, ProviderError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|error| invalid_response(format!("cannot measure provider result: {error}")))
}
