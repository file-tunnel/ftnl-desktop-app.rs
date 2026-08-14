use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use ftnl_client::FileDescriptor;
use ftnl_interfaces::FileStatus;
use ftnl_ui_components::egui_renderer;
use ftnl_ui_components::picker_machine::PickerMachineState;
use ftnl_ui_components::{FileProgress, PickerAction, PickerStage, PickerView};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::lifecycle::{Command, Completion, Effect, EffectKind, Lifecycle};
use crate::transfer::ReceiveSession;

use super::worker::{Request, RequestKind, Response, ResponseKind, Worker};

pub struct DesktopApp {
    worker: Worker,
    lifecycle: Lifecycle,
    base_url: String,
    application_id: String,
    session: Option<ReceiveSession>,
    files: Vec<FileDescriptor>,
    selected_file: Option<Uuid>,
    destination: String,
    force: bool,
    message: Option<String>,
    last_refresh: Instant,
}

impl Default for DesktopApp {
    fn default() -> Self {
        Self {
            worker: Worker::spawn(),
            lifecycle: Lifecycle::default(),
            base_url: "https://api.file-tunnel.dev".into(),
            application_id: "ftnl-desktop-app".into(),
            session: None,
            files: Vec::new(),
            selected_file: None,
            destination: String::new(),
            force: false,
            message: None,
            last_refresh: Instant::now(),
        }
    }
}

impl DesktopApp {
    pub fn new(_context: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    fn create(&mut self) {
        let Ok(Some(effect)) = self.lifecycle.dispatch(Command::Start) else {
            self.reject_action();
            return;
        };
        self.session = None;
        self.clear_files();
        self.message = None;
        self.send(
            effect,
            RequestKind::Create {
                base_url: self.base_url.clone(),
                application_id: self.application_id.clone(),
            },
        );
    }

    fn refresh(&mut self) {
        let Some((tunnel_id, capability)) = self.session_authority() else {
            self.reject_action();
            return;
        };
        let Ok(Some(effect)) = self.lifecycle.dispatch(Command::Refresh) else {
            return;
        };
        self.message = None;
        self.last_refresh = Instant::now();
        self.send(
            effect,
            RequestKind::Refresh {
                base_url: self.base_url.clone(),
                tunnel_id,
                capability,
            },
        );
    }

    fn retry(&mut self) {
        let authority = self.session_authority();
        let Ok(Some(effect)) = self.lifecycle.dispatch(Command::Retry) else {
            self.reject_action();
            return;
        };
        self.message = None;
        match (effect.kind, authority) {
            (EffectKind::Create, _) => {
                self.session = None;
                self.clear_files();
                self.send(
                    effect,
                    RequestKind::Create {
                        base_url: self.base_url.clone(),
                        application_id: self.application_id.clone(),
                    },
                );
            }
            (EffectKind::Refresh, Some((tunnel_id, capability))) => self.send(
                effect,
                RequestKind::Refresh {
                    base_url: self.base_url.clone(),
                    tunnel_id,
                    capability,
                },
            ),
            _ => self.fail_dispatch(effect),
        }
    }

    fn cancel(&mut self) {
        let Some((tunnel_id, capability)) = self.session_authority() else {
            self.reject_action();
            return;
        };
        let Ok(Some(effect)) = self.lifecycle.dispatch(Command::Cancel) else {
            self.reject_action();
            return;
        };
        self.session = None;
        self.clear_files();
        self.message = Some("Closing the secure tunnel…".into());
        self.send(
            effect,
            RequestKind::Cancel {
                base_url: self.base_url.clone(),
                tunnel_id,
                capability,
            },
        );
    }

    fn download(&mut self) {
        let Some((tunnel_id, capability)) = self.session_authority() else {
            self.reject_action();
            return;
        };
        let Some(file) = self
            .selected_file
            .and_then(|id| self.files.iter().find(|file| file.file_id == id))
            .filter(|file| is_available(&file.status))
            .cloned()
        else {
            self.message = Some("Select an available file first.".into());
            return;
        };
        let destination =
            (!self.destination.trim().is_empty()).then(|| PathBuf::from(self.destination.trim()));
        let Ok(Some(effect)) = self.lifecycle.dispatch(Command::Download) else {
            self.reject_action();
            return;
        };
        self.message = None;
        self.send(
            effect,
            RequestKind::Download {
                base_url: self.base_url.clone(),
                tunnel_id,
                capability,
                file,
                destination,
                force: self.force,
            },
        );
    }

    fn send(&mut self, effect: Effect, kind: RequestKind) {
        if !effect_matches_request(effect.kind, &kind)
            || !self.worker.send(Request {
                operation: effect.operation,
                kind,
            })
        {
            self.fail_dispatch(effect);
        }
    }

    fn fail_dispatch(&mut self, effect: Effect) {
        if self
            .lifecycle
            .complete(effect.operation, Completion::Failed)
            .is_ok()
        {
            if !self.lifecycle.state().metadata().requires_session {
                self.session = None;
            }
            self.message = Some("The background worker is unavailable. Try again.".into());
        }
    }

    fn process_responses(&mut self) {
        while let Some(response) = self.worker.try_recv() {
            self.process_response(response);
        }
    }

    fn process_response(&mut self, response: Response) {
        let Response { operation, kind } = response;
        match kind {
            ResponseKind::Created(session) => {
                if self
                    .lifecycle
                    .complete(operation, Completion::Created)
                    .is_ok()
                {
                    self.session = Some(session);
                    self.message = None;
                    self.last_refresh = Instant::now();
                }
            }
            ResponseKind::Snapshot(files) => {
                let completion = snapshot_completion(&files);
                if self.lifecycle.complete(operation, completion).is_ok() {
                    self.files = files;
                    self.selected_file = self
                        .selected_file
                        .filter(|id| self.files.iter().any(|file| file.file_id == *id));
                    if completion == Completion::AllFilesReceived {
                        self.session = None;
                        self.message = Some("All files were received.".into());
                    }
                }
            }
            ResponseKind::Downloaded(_path) => {
                if self
                    .lifecycle
                    .complete(operation, Completion::Downloaded)
                    .is_ok()
                {
                    self.message = Some("File saved successfully.".into());
                    self.refresh();
                }
            }
            ResponseKind::Cancelled => {
                if self
                    .lifecycle
                    .complete(operation, Completion::Cancelled)
                    .is_ok()
                {
                    self.session = None;
                    self.clear_files();
                    self.message = None;
                }
            }
            ResponseKind::Failed(message) => {
                if self
                    .lifecycle
                    .complete(operation, Completion::Failed)
                    .is_ok()
                {
                    if !self.lifecycle.state().metadata().requires_session {
                        self.session = None;
                    }
                    self.message = Some(message.into());
                }
            }
        }
        debug_assert!(
            self.lifecycle
                .validate_session(self.session.is_some())
                .is_ok(),
            "lifecycle and secret ownership must agree"
        );
    }

    fn session_authority(&self) -> Option<(Uuid, Zeroizing<String>)> {
        self.session.as_ref().map(|session| {
            (
                session.tunnel_id,
                Zeroizing::new(session.capability().to_owned()),
            )
        })
    }

    fn clear_files(&mut self) {
        self.files.clear();
        self.selected_file = None;
    }

    fn reject_action(&mut self) {
        self.message = Some("That action is not available in the current state.".into());
    }

    fn picker_stage(&self) -> PickerStage {
        match self.lifecycle.state() {
            PickerMachineState::Idle => PickerStage::Idle,
            PickerMachineState::Creating | PickerMachineState::Cancelling => PickerStage::Creating,
            PickerMachineState::Pairing | PickerMachineState::Refreshing => PickerStage::Pairing,
            PickerMachineState::Transferring | PickerMachineState::Downloading => {
                PickerStage::Transferring
            }
            PickerMachineState::Complete => PickerStage::Complete,
            PickerMachineState::FailedWithoutSession | PickerMachineState::FailedWithSession => {
                PickerStage::Failed
            }
        }
    }

    fn progress(&self) -> Vec<FileProgress> {
        self.files
            .iter()
            .map(|file| FileProgress {
                id: file.file_id.to_string(),
                name: file.name.clone(),
                bytes_transferred: file.bytes_transferred,
                size_bytes: file.size_bytes,
                is_complete: decode_status(&file.status).is_some_and(|status| {
                    matches!(status, FileStatus::Available | FileStatus::Downloaded)
                }),
            })
            .collect()
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_responses();
        if self.lifecycle.can(Command::Refresh)
            && self.session.is_some()
            && self.last_refresh.elapsed() >= Duration::from_secs(2)
        {
            self.refresh();
        }
        context.request_repaint_after(Duration::from_millis(250));

        let busy = self.lifecycle.is_in_flight();
        egui::SidePanel::left("settings")
            .resizable(false)
            .default_width(260.0)
            .show(context, |ui| {
                ui.heading("File Tunnel");
                ui.label("Receive files from another device without sharing an account.");
                ui.separator();
                ui.label("Service URL");
                ui.add_enabled(
                    self.session.is_none() && !busy,
                    egui::TextEdit::singleline(&mut self.base_url),
                );
                ui.label("Application ID");
                ui.add_enabled(
                    self.session.is_none() && !busy,
                    egui::TextEdit::singleline(&mut self.application_id),
                );
                ui.add_space(12.0);
                if ui
                    .add_enabled(
                        self.lifecycle.can(Command::Start),
                        egui::Button::new("New receive tunnel"),
                    )
                    .clicked()
                {
                    self.create();
                }
                if ui
                    .add_enabled(
                        self.lifecycle.can(Command::Refresh),
                        egui::Button::new("Refresh now"),
                    )
                    .clicked()
                {
                    self.refresh();
                }
                ui.separator();
                ui.small(
                    "Credentials remain in memory only and are zeroized when this session ends.",
                );
            });

        egui::CentralPanel::default().show(context, |ui| {
            let progress = self.progress();
            let pairing_uri = self.session.as_ref().map(ReceiveSession::pairing_uri);
            let expires = self
                .session
                .as_ref()
                .map(|session| session.expires_at.as_str());
            let view = PickerView {
                stage: self.picker_stage(),
                pairing_uri,
                expires_label: expires,
                files: &progress,
                failure_message: self.message.as_deref(),
            };
            if let Some(action) = egui_renderer::render(ui, view) {
                match action {
                    PickerAction::ChooseRemote => self.create(),
                    PickerAction::Retry => self.retry(),
                    PickerAction::Cancel => self.cancel(),
                    PickerAction::ChooseLocal => {
                        self.message =
                            Some("Local file browsing remains in the host application.".into());
                    }
                }
            }

            if !self.files.is_empty() {
                ui.separator();
                ui.strong("Save an available file");
                for file in &self.files {
                    if is_available(&file.status) {
                        ui.radio_value(&mut self.selected_file, Some(file.file_id), &file.name);
                    }
                }
                ui.label("Destination (blank uses the safe server filename)");
                ui.text_edit_singleline(&mut self.destination);
                ui.checkbox(&mut self.force, "Replace an existing file atomically");
                if ui
                    .add_enabled(
                        self.lifecycle.can(Command::Download),
                        egui::Button::new("Download selected file"),
                    )
                    .clicked()
                {
                    self.download();
                }
            }
            if let Some(message) = &self.message {
                ui.separator();
                ui.label(message);
            }
            if busy {
                ui.spinner();
            }
        });
    }
}

fn effect_matches_request(effect: EffectKind, request: &RequestKind) -> bool {
    matches!(
        (effect, request),
        (EffectKind::Create, RequestKind::Create { .. })
            | (EffectKind::Refresh, RequestKind::Refresh { .. })
            | (EffectKind::Download, RequestKind::Download { .. })
            | (EffectKind::Cancel, RequestKind::Cancel { .. })
    )
}

fn snapshot_completion(files: &[FileDescriptor]) -> Completion {
    if files.is_empty() {
        Completion::SnapshotEmpty
    } else if files.iter().all(|file| {
        decode_status(&file.status).is_some_and(|status| status == FileStatus::Downloaded)
    }) {
        Completion::AllFilesReceived
    } else {
        Completion::SnapshotFiles
    }
}

fn decode_status(value: &str) -> Option<FileStatus> {
    serde_json::from_value(serde_json::Value::String(value.to_owned())).ok()
}

fn is_available(value: &str) -> bool {
    decode_status(value)
        .is_some_and(|status| matches!(status, FileStatus::Available | FileStatus::Downloaded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_contract_available_states_are_downloadable() {
        assert!(is_available("available"));
        assert!(is_available("downloaded"));
        assert!(!is_available("uploading"));
        assert!(!is_available("future_status"));
    }

    #[test]
    fn request_kinds_are_bound_to_exact_effects() {
        let request = RequestKind::Create {
            base_url: String::new(),
            application_id: String::new(),
        };
        assert!(effect_matches_request(EffectKind::Create, &request));
        assert!(!effect_matches_request(EffectKind::Refresh, &request));
    }
}
