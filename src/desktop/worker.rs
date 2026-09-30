use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ftnl_client::{CreateTunnelRequest, Error as ClientError, FileDescriptor, FileTunnelClient};
use next_loggers::Logger;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::lifecycle::OperationId;
use crate::observability;
use crate::transfer::{
    save_download, validate_file_descriptor, validate_snapshot, ReceiveSession,
    MAX_FILES_PER_TUNNEL, MAX_FILE_BYTES,
};

pub struct Request {
    pub operation: OperationId,
    pub kind: RequestKind,
}

pub enum RequestKind {
    Create {
        base_url: String,
        application_id: String,
    },
    Refresh {
        base_url: String,
        tunnel_id: Uuid,
        capability: Zeroizing<String>,
    },
    Download {
        base_url: String,
        tunnel_id: Uuid,
        capability: Zeroizing<String>,
        file: FileDescriptor,
        destination: Option<PathBuf>,
        force: bool,
    },
    Cancel {
        base_url: String,
        tunnel_id: Uuid,
        capability: Zeroizing<String>,
    },
}

pub struct Response {
    pub operation: OperationId,
    pub kind: ResponseKind,
}

pub enum ResponseKind {
    Created(ReceiveSession),
    Snapshot(Vec<FileDescriptor>),
    Downloaded(PathBuf),
    Cancelled,
    Failed(&'static str),
}

pub struct Worker {
    requests: Sender<Request>,
    responses: Receiver<Response>,
}

impl Worker {
    pub fn spawn() -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("ftnl-network".into())
            .spawn(move || run(request_rx, response_tx))
            .expect("desktop network worker should start");
        Self {
            requests: request_tx,
            responses: response_rx,
        }
    }

    pub fn send(&self, request: Request) -> bool {
        self.requests.send(request).is_ok()
    }

    pub fn try_recv(&self) -> Option<Response> {
        self.responses.try_recv().ok()
    }
}

fn run(requests: Receiver<Request>, responses: Sender<Response>) {
    const ROUTINE_ID: &str = "ores-routine-MK-JvgbR4-qNcD2Az8Pd_";

    let logger = observability::logger();
    observability::event(
        &logger,
        "desktop.worker.started",
        "ores-trace-z9fNcraT6bYxxFp5ogsz-",
        ROUTINE_ID,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("desktop Tokio runtime should start");
    while let Ok(request) = requests.recv() {
        let operation = request.operation;
        let kind = runtime.block_on(handle(request.kind, &logger));
        let response = Response { operation, kind };
        if responses.send(response).is_err() {
            break;
        }
    }
    observability::event(
        &logger,
        "desktop.worker.stopped",
        "ores-trace-9ZfqebKIQFmlvUh-AcN_Q",
        ROUTINE_ID,
    );
    let _ = logger.close();
}

async fn handle(request: RequestKind, logger: &Logger) -> ResponseKind {
    const ROUTINE_ID: &str = "ores-routine-kZIb2pSDh_Tu6i-Yj5bac";

    match request {
        RequestKind::Create {
            base_url,
            application_id,
        } => {
            observability::event(
                logger,
                "tunnel.create.started",
                "ores-trace-0K5kFTygd6SBy00Z6ilfX",
                ROUTINE_ID,
            );
            let Ok(client) = client(&base_url) else {
                observability::event_error(
                    logger,
                    "tunnel.create.client_rejected",
                    "ores-trace-UhGkPwQbdpumcyvmc83jw",
                    ROUTINE_ID,
                );
                return ResponseKind::Failed("The service address is not allowed.");
            };
            let request = CreateTunnelRequest {
                application_id,
                accept: vec!["*/*".into()],
                max_files: MAX_FILES_PER_TUNNEL,
                max_file_bytes: MAX_FILE_BYTES,
                expires_in_seconds: 600,
            };
            match client.create_tunnel(&request).await {
                Ok(tunnel) => match ReceiveSession::from_tunnel(tunnel) {
                    Ok(session) => {
                        observability::event(
                            logger,
                            "tunnel.create.completed",
                            "ores-trace-sU_gLi59WwfqFshYQ8MA-",
                            ROUTINE_ID,
                        );
                        ResponseKind::Created(session)
                    }
                    Err(_) => {
                        observability::event_error(
                            logger,
                            "tunnel.create.response_rejected",
                            "ores-trace-g8856_pG04wOowwx8Cm3A",
                            ROUTINE_ID,
                        );
                        ResponseKind::Failed(
                            "The service returned a tunnel outside the receive contract.",
                        )
                    }
                },
                Err(error) => failed(
                    logger,
                    "tunnel.create.failed",
                    "ores-trace-D3AzwtoAAcaWeN7EpHhfS",
                    &error,
                ),
            }
        }
        RequestKind::Refresh {
            base_url,
            tunnel_id,
            capability,
        } => {
            let Ok(client) = client(&base_url) else {
                observability::event_error(
                    logger,
                    "tunnel.snapshot.client_rejected",
                    "ores-trace-ErOSOIAbwX4GYcnbYHmXp",
                    ROUTINE_ID,
                );
                return ResponseKind::Failed("The service address is not allowed.");
            };
            match client.snapshot(tunnel_id, capability.as_str()).await {
                Ok(snapshot) => {
                    if validate_snapshot(&snapshot.files).is_err() {
                        observability::event_error(
                            logger,
                            "tunnel.snapshot.rejected",
                            "ores-trace-7RGb-EjAe8kmpnEYfzpJj",
                            ROUTINE_ID,
                        );
                        return ResponseKind::Failed(
                            "The service returned file metadata outside the receive contract.",
                        );
                    }
                    observability::event_with_count(
                        logger,
                        "tunnel.snapshot.completed",
                        snapshot.files.len(),
                        "ores-trace--sCSbBMzt5ktP0eAyyFXq",
                        ROUTINE_ID,
                    );
                    ResponseKind::Snapshot(snapshot.files)
                }
                Err(error) => failed(
                    logger,
                    "tunnel.snapshot.failed",
                    "ores-trace-6fcK6pQcOGpVkMAhLNu-0",
                    &error,
                ),
            }
        }
        RequestKind::Download {
            base_url,
            tunnel_id,
            capability,
            file,
            destination,
            force,
        } => {
            observability::event(
                logger,
                "file.download.started",
                "ores-trace-Yda4iQ60C6sUg-Dmqw2IY",
                ROUTINE_ID,
            );
            if validate_file_descriptor(&file).is_err() {
                observability::event_error(
                    logger,
                    "file.download.metadata_rejected",
                    "ores-trace-yyPhjKYdQibKPEfjwMaUJ",
                    ROUTINE_ID,
                );
                return ResponseKind::Failed(
                    "The selected file metadata is outside the receive contract.",
                );
            }
            let Ok(client) = client(&base_url) else {
                observability::event_error(
                    logger,
                    "file.download.client_rejected",
                    "ores-trace-ZsaJmNhOu60qutOMCcgui",
                    ROUTINE_ID,
                );
                return ResponseKind::Failed("The service address is not allowed.");
            };
            let bytes = match client
                .download(tunnel_id, file.file_id, capability.as_str())
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => {
                    return failed(
                        logger,
                        "file.download.failed",
                        "ores-trace-Vn1koVq1EBpRy9deKPRyE",
                        &error,
                    )
                }
            };
            match save_download(&file, &bytes, destination.as_deref(), force) {
                Ok(path) => {
                    observability::event(
                        logger,
                        "file.download.completed",
                        "ores-trace-hWw-kz4ceN787XoAsc7w6",
                        ROUTINE_ID,
                    );
                    ResponseKind::Downloaded(path)
                }
                Err(_) => {
                    observability::event_error(
                        logger,
                        "file.persist.failed",
                        "ores-trace-7EAyj7tKD2ktPKgiZqO4f",
                        ROUTINE_ID,
                    );
                    ResponseKind::Failed(
                        "The downloaded file could not be safely written at that location.",
                    )
                }
            }
        }
        RequestKind::Cancel {
            base_url,
            tunnel_id,
            capability,
        } => {
            let Ok(client) = client(&base_url) else {
                observability::event_error(
                    logger,
                    "tunnel.cancel.client_rejected",
                    "ores-trace-nvbe4gtOJVjbIgfVnU9Wo",
                    ROUTINE_ID,
                );
                return ResponseKind::Failed("The service address is not allowed.");
            };
            match client.cancel(tunnel_id, capability.as_str()).await {
                Ok(()) => {
                    observability::event(
                        logger,
                        "tunnel.cancel.completed",
                        "ores-trace-V9Ri2WmXHTIHjhFDT-UBt",
                        ROUTINE_ID,
                    );
                    ResponseKind::Cancelled
                }
                Err(error) => failed(
                    logger,
                    "tunnel.cancel.failed",
                    "ores-trace-BVZC_ODNAjWj-TN8DcuCF",
                    &error,
                ),
            }
        }
    }
}

fn client(base_url: &str) -> Result<FileTunnelClient, ClientError> {
    FileTunnelClient::with_timeout(base_url, Duration::from_secs(30))
}

/// Logs the failure of an upstream call and then returns the same user-facing
/// `ResponseKind::Failed` it always returned. `trace_id` is the caller's inline
/// `ores-trace-` literal; the `ClientError` detail is never logged.
fn failed(
    logger: &Logger,
    event: &'static str,
    trace_id: &'static str,
    error: &ClientError,
) -> ResponseKind {
    const ROUTINE_ID: &str = "ores-routine-uvRjbNn4tiqmclZSTsCTn";

    observability::event_error(logger, event, trace_id, ROUTINE_ID);
    match error {
        ClientError::Api { status, .. } if matches!(status.as_u16(), 404 | 410) => {
            ResponseKind::Failed("The tunnel expired or is no longer available.")
        }
        ClientError::Api { status, .. } if status.as_u16() == 401 || status.as_u16() == 403 => {
            ResponseKind::Failed("This session is no longer authorized.")
        }
        ClientError::InvalidBaseUrl(_)
        | ClientError::UnsupportedScheme(_)
        | ClientError::InsecureTransport(_)
        | ClientError::InvalidTimeout => {
            ResponseKind::Failed("The service address is not allowed.")
        }
        ClientError::Transport(_) | ClientError::Api { .. } => {
            ResponseKind::Failed("The network request failed. Check the connection and try again.")
        }
    }
}
