use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use ftnl_client::FileDescriptor;
use ftnl_interfaces::FileStatus;
use ftnl_ui_components::egui_renderer;
use ftnl_ui_components::{FileProgress, PickerAction, PickerStage, PickerView};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::transfer::ReceiveSession;

use super::worker::{Request, Response, Worker};

pub struct DesktopApp {
    worker: Worker,
    base_url: String,
    application_id: String,
    session: Option<ReceiveSession>,
    files: Vec<FileDescriptor>,
    selected_file: Option<Uuid>,
    destination: String,
    force: bool,
    stage: PickerStage,
    busy: bool,
    message: Option<String>,
    last_refresh: Instant,
}

impl Default for DesktopApp {
    fn default() -> Self {
        Self {
            worker: Worker::spawn(),
            base_url: "https://api.file-tunnel.dev".into(),
            application_id: "ftnl-desktop-app".into(),
            session: None,
            files: Vec::new(),
            selected_file: None,
            destination: String::new(),
            force: false,
            stage: PickerStage::Idle,
            busy: false,
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
        if self.busy {
            return;
        }
        self.stage = PickerStage::Creating;
        self.message = None;
        self.files.clear();
        self.selected_file = None;
        self.session = None;
        self.busy = self.worker.send(Request::Create {
            base_url: self.base_url.clone(),
            application_id: self.application_id.clone(),
        });
    }

    fn refresh(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = self.worker.send(Request::Refresh {
            base_url: self.base_url.clone(),
            tunnel_id: session.tunnel_id,
            capability: Zeroizing::new(session.capability().to_owned()),
        });
        self.last_refresh = Instant::now();
    }

    fn cancel(&mut self) {
        let Some(session) = self.session.take() else {
            self.reset();
            return;
        };
        self.busy = self.worker.send(Request::Cancel {
            base_url: self.base_url.clone(),
            tunnel_id: session.tunnel_id,
            capability: Zeroizing::new(session.capability().to_owned()),
        });
        self.files.clear();
        self.selected_file = None;
    }

    fn download(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let Some(file) = self
            .selected_file
            .and_then(|id| self.files.iter().find(|file| file.file_id == id))
            .cloned()
        else {
            self.message = Some("Select an available file first.".into());
            return;
        };
        let destination =
            (!self.destination.trim().is_empty()).then(|| PathBuf::from(self.destination.trim()));
        self.busy = self.worker.send(Request::Download {
            base_url: self.base_url.clone(),
            tunnel_id: session.tunnel_id,
            capability: Zeroizing::new(session.capability().to_owned()),
            file,
            destination,
            force: self.force,
        });
        self.message = None;
    }

    fn reset(&mut self) {
        self.session = None;
        self.files.clear();
        self.selected_file = None;
        self.stage = PickerStage::Idle;
        self.busy = false;
    }

    fn process_responses(&mut self) {
        while let Some(response) = self.worker.try_recv() {
            self.busy = false;
            match response {
                Response::Created(session) => {
                    self.session = Some(session);
                    self.stage = PickerStage::Pairing;
                    self.last_refresh = Instant::now();
                }
                Response::Snapshot(files) => {
                    self.files = files;
                    self.stage = if self.files.is_empty() {
                        PickerStage::Pairing
                    } else {
                        PickerStage::Transferring
                    };
                }
                Response::Downloaded(path) => {
                    self.message = Some(format!("Saved to {}", path.display()));
                    self.refresh();
                }
                Response::Cancelled => self.reset(),
                Response::Failed(message) => {
                    self.message = Some(message.into());
                    self.stage = PickerStage::Failed;
                }
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
        if self.session.is_some()
            && !self.busy
            && self.last_refresh.elapsed() >= Duration::from_secs(2)
            && !matches!(self.stage, PickerStage::Failed | PickerStage::Complete)
        {
            self.refresh();
        }
        context.request_repaint_after(Duration::from_millis(250));

        egui::SidePanel::left("settings")
            .resizable(false)
            .default_width(260.0)
            .show(context, |ui| {
                ui.heading("File Tunnel");
                ui.label("Receive files from another device without sharing an account.");
                ui.separator();
                ui.label("Service URL");
                ui.add_enabled(
                    self.session.is_none() && !self.busy,
                    egui::TextEdit::singleline(&mut self.base_url),
                );
                ui.label("Application ID");
                ui.add_enabled(
                    self.session.is_none() && !self.busy,
                    egui::TextEdit::singleline(&mut self.application_id),
                );
                ui.add_space(12.0);
                if ui
                    .add_enabled(!self.busy, egui::Button::new("New receive tunnel"))
                    .clicked()
                {
                    self.create();
                }
                if self.session.is_some()
                    && ui
                        .add_enabled(!self.busy, egui::Button::new("Refresh now"))
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
                stage: self.stage,
                pairing_uri,
                expires_label: expires,
                files: &progress,
                failure_message: self.message.as_deref(),
            };
            if let Some(action) = egui_renderer::render(ui, view) {
                match action {
                    PickerAction::ChooseRemote => self.create(),
                    PickerAction::Retry if self.session.is_some() => self.refresh(),
                    PickerAction::Retry => self.create(),
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
                    .add_enabled(!self.busy, egui::Button::new("Download selected file"))
                    .clicked()
                {
                    self.download();
                }
            }
            if let Some(message) = &self.message {
                ui.separator();
                ui.label(message);
            }
            if self.busy {
                ui.spinner();
            }
        });
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
}
