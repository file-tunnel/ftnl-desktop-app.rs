use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ftnl_client::{CreateTunnelRequest, Error as ClientError, FileDescriptor, FileTunnelClient};
use next_loggers::Logger;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::lifecycle::OperationId;
use crate::observability;
use crate::transfer::{save_download, ReceiveSession};

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
    let logger = observability::logger();
    observability::event(&logger, "desktop.worker.started");
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
    observability::event(&logger, "desktop.worker.stopped");
    let _ = logger.close();
}

async fn handle(request: RequestKind, logger: &Logger) -> ResponseKind {
    match request {
        RequestKind::Create {
            base_url,
            application_id,
        } => {
            observability::event(logger, "tunnel.create.started");
            let Ok(client) = client(&base_url) else {
                return ResponseKind::Failed("The service address is not allowed.");
            };
            let request = CreateTunnelRequest {
                application_id,
                accept: vec!["*/*".into()],
                max_files: 10,
                max_file_bytes: 50 * 1024 * 1024,
                expires_in_seconds: 600,
            };
            match client.create_tunnel(&request).await {
                Ok(tunnel) => {
                    observability::event(logger, "tunnel.create.completed");
                    ResponseKind::Created(ReceiveSession::from_tunnel(tunnel))
                }
                Err(error) => failed(logger, "tunnel.create.failed", &error),
            }
        }
        RequestKind::Refresh {
            base_url,
            tunnel_id,
            capability,
        } => {
            let Ok(client) = client(&base_url) else {
                return ResponseKind::Failed("The service address is not allowed.");
            };
            match client.snapshot(tunnel_id, capability.as_str()).await {
                Ok(snapshot) => {
                    observability::event_with_count(
                        logger,
                        "tunnel.snapshot.completed",
                        snapshot.files.len(),
                    );
                    ResponseKind::Snapshot(snapshot.files)
                }
                Err(error) => failed(logger, "tunnel.snapshot.failed", &error),
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
            observability::event(logger, "file.download.started");
            let Ok(client) = client(&base_url) else {
                return ResponseKind::Failed("The service address is not allowed.");
            };
            let bytes = match client
                .download(tunnel_id, file.file_id, capability.as_str())
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => return failed(logger, "file.download.failed", &error),
            };
            match save_download(&file, &bytes, destination.as_deref(), force) {
                Ok(path) => {
                    observability::event(logger, "file.download.completed");
                    ResponseKind::Downloaded(path)
                }
                Err(_) => {
                    observability::event(logger, "file.persist.failed");
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
                return ResponseKind::Failed("The service address is not allowed.");
            };
            match client.cancel(tunnel_id, capability.as_str()).await {
                Ok(()) => {
                    observability::event(logger, "tunnel.cancel.completed");
                    ResponseKind::Cancelled
                }
                Err(error) => failed(logger, "tunnel.cancel.failed", &error),
            }
        }
    }
}

fn client(base_url: &str) -> Result<FileTunnelClient, ClientError> {
    FileTunnelClient::with_timeout(base_url, Duration::from_secs(30))
}

fn failed(logger: &Logger, event: &'static str, error: &ClientError) -> ResponseKind {
    observability::event(logger, event);
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
