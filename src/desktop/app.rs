use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use arboard::Clipboard;
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
use crate::workspace::{
    content_sha256_hex, CaptureState, ClipboardItem, ClipboardWorkspace, RetentionPolicy,
    WorkspaceAction,
};

use super::shell::{NativeTray, ShellAction, ShellState};
use super::worker::{Request, RequestKind, Response, ResponseKind, Worker};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum WorkspacePage {
    #[default]
    Clipboard,
    Receive,
    Privacy,
}

pub struct DesktopApp {
    tray: Option<NativeTray>,
    shell: ShellState,
    clipboard: Option<Clipboard>,
    workspace: ClipboardWorkspace,
    page: WorkspacePage,
    last_clipboard_hash: Option<String>,
    last_clipboard_poll: Instant,
    workspace_message: Option<String>,
    source_exclusion_input: String,
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
            tray: None,
            shell: ShellState::default(),
            clipboard: None,
            workspace: ClipboardWorkspace::default(),
            page: WorkspacePage::default(),
            last_clipboard_hash: None,
            last_clipboard_poll: Instant::now(),
            workspace_message: None,
            source_exclusion_input: String::new(),
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
        Self {
            tray: NativeTray::new().ok(),
            clipboard: Clipboard::new().ok(),
            ..Self::default()
        }
    }

    fn apply_workspace_action(&mut self, action: WorkspaceAction) -> bool {
        let result = self.workspace.apply(
            Uuid::new_v4(),
            self.workspace.revision(),
            action,
            SystemTime::now(),
        );
        match result {
            Ok(()) => {
                self.workspace_message = None;
                true
            }
            Err(error) => {
                self.workspace_message = Some(error.to_string());
                false
            }
        }
    }

    fn set_capture_active(&mut self, active: bool) {
        let expected = if active {
            CaptureState::Active
        } else {
            CaptureState::Paused
        };
        if self.workspace.capture_state() != expected {
            let action = if active {
                WorkspaceAction::ResumeCapture
            } else {
                WorkspaceAction::PauseCapture
            };
            self.apply_workspace_action(action);
        }
        self.shell.capture_active = self.workspace.capture_state() == CaptureState::Active;
    }

    fn capture_clipboard_text(&mut self, report_empty: bool) {
        if self.workspace.capture_state() != CaptureState::Active {
            return;
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            if report_empty {
                self.workspace_message =
                    Some("Clipboard access is unavailable on this system.".into());
            }
            return;
        };
        let Ok(text) = clipboard.get_text() else {
            if report_empty {
                self.workspace_message = Some("The clipboard does not contain plain text.".into());
            }
            return;
        };
        if text.is_empty() {
            return;
        }

        let content_sha256 = content_sha256_hex(&text);
        if self.last_clipboard_hash.as_ref() == Some(&content_sha256) {
            if report_empty {
                self.workspace_message = Some("That clipboard item is already current.".into());
            }
            return;
        }
        let captured_at = SystemTime::now();
        let action = WorkspaceAction::IngestText {
            item_id: Uuid::new_v4(),
            byte_size: text.len(),
            text,
            content_sha256: content_sha256.clone(),
            captured_at,
            source_fingerprint: None,
        };
        if self.apply_workspace_action(action) {
            self.last_clipboard_hash = Some(content_sha256);
        }
    }

    fn copy_clipboard_item(&mut self, item: &ClipboardItem) {
        let Some(clipboard) = self.clipboard.as_mut() else {
            self.workspace_message = Some("Clipboard access is unavailable on this system.".into());
            return;
        };
        if clipboard.set_text(item.text.clone()).is_ok() {
            self.last_clipboard_hash = Some(item.content_sha256.clone());
            self.workspace_message = Some("Copied to the system clipboard.".into());
        } else {
            self.workspace_message = Some("The clipboard could not be updated.".into());
        }
    }

    fn process_shell(&mut self, context: &egui::Context) {
        let actions = self
            .tray
            .as_ref()
            .map(NativeTray::poll_actions)
            .unwrap_or_default();
        for action in actions {
            match action {
                ShellAction::OpenWindow => {
                    self.shell.apply(action);
                    context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    context.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                ShellAction::HideWindow => {
                    self.shell.apply(action);
                    context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
                ShellAction::PauseCapture => self.set_capture_active(false),
                ShellAction::ResumeCapture => self.set_capture_active(true),
                ShellAction::Quit => {
                    self.shell.apply(action);
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        if context.input(|input| input.viewport().close_requested())
            && self.shell.on_close_requested(self.tray.is_some())
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn poll_clipboard(&mut self) {
        if self.workspace.capture_state() == CaptureState::Active
            && self.last_clipboard_poll.elapsed() >= Duration::from_millis(750)
        {
            self.last_clipboard_poll = Instant::now();
            self.capture_clipboard_text(false);
        }
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

    fn render_sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("File Tunnel");
        ui.label("Clipboard workspace and secure peer-to-peer receiving.");
        ui.add_space(16.0);

        ui.selectable_value(&mut self.page, WorkspacePage::Clipboard, "Clipboard");
        ui.selectable_value(&mut self.page, WorkspacePage::Receive, "Receive files");
        ui.selectable_value(
            &mut self.page,
            WorkspacePage::Privacy,
            "Privacy & retention",
        );

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            if self.tray.is_some() {
                ui.small("Closing this window keeps File Tunnel in the system tray.");
            } else {
                ui.small("System tray unavailable. Closing this window quits the app.");
            }
            let capture = match self.workspace.capture_state() {
                CaptureState::Active => "Clipboard capture: active",
                CaptureState::Paused => "Clipboard capture: paused",
            };
            ui.small(capture);
            ui.separator();
        });
    }

    fn render_clipboard(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Clipboard");
            ui.add_space(8.0);
            let active = self.workspace.capture_state() == CaptureState::Active;
            if ui
                .button(if active {
                    "Pause capture"
                } else {
                    "Resume capture"
                })
                .clicked()
            {
                self.set_capture_active(!active);
            }
            if ui
                .add_enabled(active, egui::Button::new("Capture now"))
                .clicked()
            {
                self.capture_clipboard_text(true);
            }
        });
        ui.label(
            "Text capture is opt-in, bounded, deduplicated, and held in memory for this session.",
        );
        ui.add_space(12.0);

        let mut search = self.workspace.search_query().to_owned();
        ui.horizontal(|ui| {
            ui.label("Search");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut search)
                        .hint_text("Find clipboard text")
                        .desired_width(360.0),
                )
                .changed()
            {
                self.apply_workspace_action(WorkspaceAction::SetSearch(search));
            }
            if ui
                .add_enabled(
                    self.workspace.items().iter().any(|item| !item.is_pinned),
                    egui::Button::new("Clear unpinned"),
                )
                .clicked()
            {
                self.apply_workspace_action(WorkspaceAction::ClearUnpinned);
            }
        });
        ui.separator();

        let items = self
            .workspace
            .visible_items()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if items.is_empty() {
            ui.add_space(32.0);
            ui.vertical_centered(|ui| {
                ui.heading("No clipboard items");
                ui.label(if self.workspace.search_query().is_empty() {
                    "Resume capture to begin a private, in-memory history."
                } else {
                    "No captured text matches this search."
                });
            });
        } else {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for item in items {
                    let mut copy = false;
                    let mut action = None;
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.strong(if item.is_pinned {
                                "Pinned"
                            } else {
                                "Clipboard"
                            });
                            ui.weak(format!("{} bytes", item.byte_size));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Delete").clicked() {
                                        action = Some(WorkspaceAction::DeleteItem(item.item_id));
                                    }
                                    if ui
                                        .small_button(if item.is_pinned { "Unpin" } else { "Pin" })
                                        .clicked()
                                    {
                                        action = Some(if item.is_pinned {
                                            WorkspaceAction::UnpinItem(item.item_id)
                                        } else {
                                            WorkspaceAction::PinItem(item.item_id)
                                        });
                                    }
                                    if ui.small_button("Copy").clicked() {
                                        copy = true;
                                    }
                                },
                            );
                        });
                        ui.add_space(6.0);
                        ui.label(clipboard_preview(&item.text));
                    });
                    if copy {
                        self.copy_clipboard_item(&item);
                    }
                    if let Some(action) = action {
                        self.apply_workspace_action(action);
                    }
                    ui.add_space(8.0);
                }
            });
        }

        if let Some(message) = &self.workspace_message {
            ui.separator();
            ui.label(message);
        }
    }

    fn render_receive(&mut self, ui: &mut egui::Ui) {
        ui.heading("Receive files");
        ui.label("Create a short-lived tunnel and receive files without sharing an account.");
        ui.add_space(12.0);

        let busy = self.lifecycle.is_in_flight();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label("Service URL");
                    ui.add_enabled(
                        self.session.is_none() && !busy,
                        egui::TextEdit::singleline(&mut self.base_url).desired_width(320.0),
                    );
                });
                ui.vertical(|ui| {
                    ui.label("Application ID");
                    ui.add_enabled(
                        self.session.is_none() && !busy,
                        egui::TextEdit::singleline(&mut self.application_id).desired_width(220.0),
                    );
                });
            });
            ui.add_space(8.0);
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
            ui.small("Session capabilities stay in memory and are zeroized when the session ends.");
        });
        ui.add_space(12.0);

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
    }

    fn render_privacy(&mut self, ui: &mut egui::Ui) {
        ui.heading("Privacy & retention");
        ui.label(
            "Capture begins paused on every launch. History currently remains in memory only.",
        );
        ui.add_space(12.0);

        let current = self.workspace.retention();
        let mut max_items = current.max_items;
        let mut max_age_days = current.max_age_days;
        let mut deduplicate = current.deduplicate;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("Retention policy");
            ui.add(egui::Slider::new(&mut max_items, 1..=10_000).text("maximum unpinned items"));
            ui.add(egui::Slider::new(&mut max_age_days, 1..=365).text("maximum age in days"));
            ui.checkbox(&mut deduplicate, "Deduplicate matching content");
            let changed = max_items != current.max_items
                || max_age_days != current.max_age_days
                || deduplicate != current.deduplicate;
            if ui
                .add_enabled(changed, egui::Button::new("Apply retention policy"))
                .clicked()
            {
                self.apply_workspace_action(WorkspaceAction::SetRetention(RetentionPolicy {
                    max_items,
                    max_age_days,
                    deduplicate,
                }));
            }
            ui.small("Pinned items are exempt from automatic age and count cleanup.");
        });
        ui.add_space(12.0);

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("Excluded source fingerprints");
            ui.label(
                "The policy rejects matching SHA-256 source fingerprints. The generic clipboard adapter does not yet identify the source application.",
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.source_exclusion_input)
                        .hint_text("sha256: followed by 64 lowercase hex characters")
                        .desired_width(440.0),
                );
                if ui.button("Add").clicked() {
                    let mut values = self
                        .workspace
                        .excluded_source_fingerprints()
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    values.push(self.source_exclusion_input.trim().to_owned());
                    if self.apply_workspace_action(WorkspaceAction::SetSourceExclusions(values)) {
                        self.source_exclusion_input.clear();
                    }
                }
            });

            let exclusions = self
                .workspace
                .excluded_source_fingerprints()
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            for fingerprint in exclusions {
                ui.horizontal(|ui| {
                    ui.monospace(source_fingerprint_preview(&fingerprint));
                    if ui.small_button("Remove").clicked() {
                        let values = self
                            .workspace
                            .excluded_source_fingerprints()
                            .iter()
                            .filter(|value| *value != &fingerprint)
                            .cloned()
                            .collect::<Vec<_>>();
                        self.apply_workspace_action(WorkspaceAction::SetSourceExclusions(values));
                    }
                });
            }
        });
        ui.add_space(12.0);
        ui.small(
            "Bluetooth and proximity transports may help discover peers, but they never establish identity or authorization on their own.",
        );
        if let Some(message) = &self.workspace_message {
            ui.separator();
            ui.label(message);
        }
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_shell(context);
        self.process_responses();
        self.poll_clipboard();
        if self.lifecycle.can(Command::Refresh)
            && self.session.is_some()
            && self.last_refresh.elapsed() >= Duration::from_secs(2)
        {
            self.refresh();
        }
        context.request_repaint_after(Duration::from_millis(250));

        egui::SidePanel::left("workspace-navigation")
            .resizable(false)
            .default_width(220.0)
            .show(context, |ui| self.render_sidebar(ui));

        egui::CentralPanel::default().show(context, |ui| match self.page {
            WorkspacePage::Clipboard => self.render_clipboard(ui),
            WorkspacePage::Receive => self.render_receive(ui),
            WorkspacePage::Privacy => self.render_privacy(ui),
        });
    }
}

fn clipboard_preview(text: &str) -> String {
    const MAX_CHARACTERS: usize = 240;
    let mut preview = text.chars().take(MAX_CHARACTERS).collect::<String>();
    if text.chars().count() > MAX_CHARACTERS {
        preview.push('…');
    }
    preview
}

fn source_fingerprint_preview(value: &str) -> String {
    if value.len() > 24 {
        format!("{}…{}", &value[..15], &value[value.len() - 8..])
    } else {
        value.to_owned()
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
