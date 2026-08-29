//! Pure clipboard-workspace reducer shared by the native window and tray shell.
//!
//! Clipboard text is local user content. This module never logs, persists,
//! transmits, or formats content into errors.

use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const DESKTOP_FEATURE_IDS: [&str; 12] = [
    "clipboard.capture.pause",
    "clipboard.clear_unpinned",
    "clipboard.deduplicate",
    "clipboard.delete",
    "clipboard.history.text",
    "clipboard.pin",
    "clipboard.retention",
    "clipboard.search",
    "desktop.tray.lifecycle",
    "desktop.window.close_to_tray",
    "desktop.window.regular",
    "privacy.source_exclusions",
];

const MAX_TEXT_CHARACTERS: usize = 65_536;
const MAX_TEXT_BYTES: usize = 1_048_576;
const MAX_SEARCH_CHARACTERS: usize = 256;
const MAX_SOURCE_EXCLUSIONS: usize = 256;
const MAX_PROCESSED_COMMANDS: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureState {
    Active,
    Paused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionPolicy {
    pub max_items: usize,
    pub max_age_days: u16,
    pub deduplicate: bool,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_items: 250,
            max_age_days: 30,
            deduplicate: true,
        }
    }
}

impl RetentionPolicy {
    fn validate(self) -> Result<Self, WorkspaceError> {
        if !(1..=10_000).contains(&self.max_items) || !(1..=365).contains(&self.max_age_days) {
            Err(WorkspaceError::InvalidRetention)
        } else {
            Ok(self)
        }
    }

    fn max_age(self) -> Duration {
        Duration::from_secs(u64::from(self.max_age_days) * 24 * 60 * 60)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardItem {
    pub item_id: Uuid,
    pub text: String,
    pub content_sha256: String,
    pub byte_size: usize,
    pub captured_at: SystemTime,
    pub is_pinned: bool,
    pub source_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceAction {
    PauseCapture,
    ResumeCapture,
    SetSearch(String),
    PinItem(Uuid),
    UnpinItem(Uuid),
    DeleteItem(Uuid),
    ClearUnpinned,
    SetRetention(RetentionPolicy),
    SetSourceExclusions(Vec<String>),
    IngestText {
        item_id: Uuid,
        text: String,
        content_sha256: String,
        byte_size: usize,
        captured_at: SystemTime,
        source_fingerprint: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorkspaceError {
    #[error("clipboard capture is paused")]
    CapturePaused,
    #[error("the command was already applied")]
    DuplicateCommand,
    #[error("the clipboard source is excluded")]
    ExcludedSource,
    #[error("clipboard content metadata is invalid")]
    InvalidContentHash,
    #[error("clipboard content is outside the supported bounds")]
    InvalidContentSize,
    #[error("the retention policy is outside the supported bounds")]
    InvalidRetention,
    #[error("the search query is outside the supported bounds")]
    InvalidSearch,
    #[error("a source fingerprint is malformed")]
    InvalidSourceFingerprint,
    #[error("the clipboard item does not exist")]
    ItemNotFound,
    #[error("the workspace revision changed")]
    RevisionConflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardWorkspace {
    revision: u64,
    capture_state: CaptureState,
    retention: RetentionPolicy,
    search_query: String,
    excluded_source_fingerprints: BTreeSet<String>,
    items: Vec<ClipboardItem>,
    processed_commands: VecDeque<Uuid>,
}

impl Default for ClipboardWorkspace {
    fn default() -> Self {
        Self {
            revision: 0,
            capture_state: CaptureState::Paused,
            retention: RetentionPolicy::default(),
            search_query: String::new(),
            excluded_source_fingerprints: BTreeSet::new(),
            items: Vec::new(),
            processed_commands: VecDeque::new(),
        }
    }
}

impl ClipboardWorkspace {
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub const fn capture_state(&self) -> CaptureState {
        self.capture_state
    }

    pub const fn retention(&self) -> RetentionPolicy {
        self.retention
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn items(&self) -> &[ClipboardItem] {
        &self.items
    }

    pub fn excluded_source_fingerprints(&self) -> &BTreeSet<String> {
        &self.excluded_source_fingerprints
    }

    pub fn visible_items(&self) -> Vec<&ClipboardItem> {
        let query = self.search_query.to_lowercase();
        let matches = |item: &ClipboardItem| {
            query.is_empty() || item.text.to_lowercase().contains(query.as_str())
        };
        self.items
            .iter()
            .filter(|item| item.is_pinned)
            .filter(|item| matches(item))
            .chain(
                self.items
                    .iter()
                    .filter(|item| !item.is_pinned)
                    .filter(|item| matches(item)),
            )
            .collect()
    }

    pub fn apply(
        &mut self,
        command_id: Uuid,
        expected_revision: u64,
        action: WorkspaceAction,
        now: SystemTime,
    ) -> Result<(), WorkspaceError> {
        if self.processed_commands.contains(&command_id) {
            return Err(WorkspaceError::DuplicateCommand);
        }
        if expected_revision != self.revision {
            return Err(WorkspaceError::RevisionConflict);
        }

        match action {
            WorkspaceAction::PauseCapture => self.capture_state = CaptureState::Paused,
            WorkspaceAction::ResumeCapture => self.capture_state = CaptureState::Active,
            WorkspaceAction::SetSearch(query) => {
                if query.chars().count() > MAX_SEARCH_CHARACTERS {
                    return Err(WorkspaceError::InvalidSearch);
                }
                self.search_query = query;
            }
            WorkspaceAction::PinItem(item_id) => {
                self.item_mut(item_id)?.is_pinned = true;
            }
            WorkspaceAction::UnpinItem(item_id) => {
                self.item_mut(item_id)?.is_pinned = false;
                self.enforce_retention(now);
            }
            WorkspaceAction::DeleteItem(item_id) => {
                let before = self.items.len();
                self.items.retain(|item| item.item_id != item_id);
                if before == self.items.len() {
                    return Err(WorkspaceError::ItemNotFound);
                }
            }
            WorkspaceAction::ClearUnpinned => {
                self.items.retain(|item| item.is_pinned);
            }
            WorkspaceAction::SetRetention(policy) => {
                self.retention = policy.validate()?;
                self.enforce_retention(now);
            }
            WorkspaceAction::SetSourceExclusions(fingerprints) => {
                if fingerprints.len() > MAX_SOURCE_EXCLUSIONS
                    || fingerprints
                        .iter()
                        .any(|value| !is_source_fingerprint(value))
                {
                    return Err(WorkspaceError::InvalidSourceFingerprint);
                }
                self.excluded_source_fingerprints = fingerprints.into_iter().collect();
            }
            WorkspaceAction::IngestText {
                item_id,
                text,
                content_sha256,
                byte_size,
                captured_at,
                source_fingerprint,
            } => {
                self.ingest_text(
                    item_id,
                    text,
                    content_sha256,
                    byte_size,
                    captured_at,
                    source_fingerprint,
                    now,
                )?;
            }
        }

        self.revision = self.revision.saturating_add(1);
        self.processed_commands.push_back(command_id);
        if self.processed_commands.len() > MAX_PROCESSED_COMMANDS {
            self.processed_commands.pop_front();
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn ingest_text(
        &mut self,
        item_id: Uuid,
        text: String,
        content_sha256: String,
        byte_size: usize,
        captured_at: SystemTime,
        source_fingerprint: Option<String>,
        now: SystemTime,
    ) -> Result<(), WorkspaceError> {
        if self.capture_state != CaptureState::Active {
            return Err(WorkspaceError::CapturePaused);
        }
        if text.is_empty()
            || text.chars().count() > MAX_TEXT_CHARACTERS
            || text.len() > MAX_TEXT_BYTES
            || byte_size != text.len()
        {
            return Err(WorkspaceError::InvalidContentSize);
        }
        if content_sha256 != content_sha256_hex(&text) {
            return Err(WorkspaceError::InvalidContentHash);
        }
        if source_fingerprint
            .as_deref()
            .is_some_and(|value| !is_source_fingerprint(value))
        {
            return Err(WorkspaceError::InvalidSourceFingerprint);
        }
        if source_fingerprint
            .as_ref()
            .is_some_and(|value| self.excluded_source_fingerprints.contains(value))
        {
            return Err(WorkspaceError::ExcludedSource);
        }

        if self.retention.deduplicate {
            if let Some(index) = self
                .items
                .iter()
                .position(|item| item.content_sha256 == content_sha256)
            {
                let mut item = self.items.remove(index);
                item.captured_at = captured_at;
                item.source_fingerprint = source_fingerprint;
                self.items.insert(0, item);
                self.enforce_retention(now);
                return Ok(());
            }
        }

        self.items.insert(
            0,
            ClipboardItem {
                item_id,
                text,
                content_sha256,
                byte_size,
                captured_at,
                is_pinned: false,
                source_fingerprint,
            },
        );
        self.enforce_retention(now);
        Ok(())
    }

    fn item_mut(&mut self, item_id: Uuid) -> Result<&mut ClipboardItem, WorkspaceError> {
        self.items
            .iter_mut()
            .find(|item| item.item_id == item_id)
            .ok_or(WorkspaceError::ItemNotFound)
    }

    fn enforce_retention(&mut self, now: SystemTime) {
        let max_age = self.retention.max_age();
        self.items.retain(|item| {
            item.is_pinned
                || now
                    .duration_since(item.captured_at)
                    .map_or(true, |age| age <= max_age)
        });

        while self.items.iter().filter(|item| !item.is_pinned).count() > self.retention.max_items {
            if let Some(index) = self.items.iter().rposition(|item| !item.is_pinned) {
                self.items.remove(index);
            } else {
                break;
            }
        }
    }
}

pub fn content_sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn is_source_fingerprint(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(day * 24 * 60 * 60)
    }

    fn apply(
        workspace: &mut ClipboardWorkspace,
        action: WorkspaceAction,
        now: SystemTime,
    ) -> Result<(), WorkspaceError> {
        workspace.apply(Uuid::new_v4(), workspace.revision(), action, now)
    }

    fn ingest(item_id: Uuid, text: &str, captured_at: SystemTime) -> WorkspaceAction {
        WorkspaceAction::IngestText {
            item_id,
            text: text.into(),
            content_sha256: content_sha256_hex(text),
            byte_size: text.len(),
            captured_at,
            source_fingerprint: None,
        }
    }

    #[test]
    fn capture_is_explicit_and_revision_conflicts_do_not_mutate() {
        let mut workspace = ClipboardWorkspace::default();
        assert_eq!(workspace.capture_state(), CaptureState::Paused);
        assert_eq!(
            apply(&mut workspace, ingest(Uuid::new_v4(), "one", at(1)), at(1)),
            Err(WorkspaceError::CapturePaused)
        );
        assert_eq!(workspace.revision(), 0);

        apply(&mut workspace, WorkspaceAction::ResumeCapture, at(1)).unwrap();
        let before = workspace.clone();
        assert_eq!(
            workspace.apply(Uuid::new_v4(), 0, WorkspaceAction::PauseCapture, at(1)),
            Err(WorkspaceError::RevisionConflict)
        );
        assert_eq!(workspace, before);
    }

    #[test]
    fn deduplication_moves_existing_content_and_preserves_pin() {
        let mut workspace = ClipboardWorkspace::default();
        apply(&mut workspace, WorkspaceAction::ResumeCapture, at(1)).unwrap();
        let first = Uuid::new_v4();
        apply(&mut workspace, ingest(first, "same text", at(1)), at(1)).unwrap();
        apply(&mut workspace, WorkspaceAction::PinItem(first), at(1)).unwrap();
        apply(
            &mut workspace,
            ingest(Uuid::new_v4(), "same text", at(2)),
            at(2),
        )
        .unwrap();

        assert_eq!(workspace.items().len(), 1);
        assert_eq!(workspace.items()[0].item_id, first);
        assert!(workspace.items()[0].is_pinned);
        assert_eq!(workspace.items()[0].captured_at, at(2));
    }

    #[test]
    fn search_is_case_insensitive_and_pins_sort_first() {
        let mut workspace = ClipboardWorkspace::default();
        apply(&mut workspace, WorkspaceAction::ResumeCapture, at(1)).unwrap();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        apply(&mut workspace, ingest(first, "Alpha result", at(1)), at(1)).unwrap();
        apply(
            &mut workspace,
            ingest(second, "another ALPHA", at(2)),
            at(2),
        )
        .unwrap();
        apply(&mut workspace, WorkspaceAction::PinItem(first), at(2)).unwrap();
        apply(
            &mut workspace,
            WorkspaceAction::SetSearch("alpha".into()),
            at(2),
        )
        .unwrap();

        let visible = workspace.visible_items();
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].item_id, first);
        assert_eq!(visible[1].item_id, second);
    }

    #[test]
    fn clear_and_retention_never_remove_pinned_items() {
        let mut workspace = ClipboardWorkspace::default();
        apply(&mut workspace, WorkspaceAction::ResumeCapture, at(1)).unwrap();
        let pinned = Uuid::new_v4();
        apply(&mut workspace, ingest(pinned, "pinned", at(1)), at(1)).unwrap();
        apply(&mut workspace, WorkspaceAction::PinItem(pinned), at(1)).unwrap();
        apply(
            &mut workspace,
            ingest(Uuid::new_v4(), "expired", at(1)),
            at(1),
        )
        .unwrap();
        apply(
            &mut workspace,
            WorkspaceAction::SetRetention(RetentionPolicy {
                max_items: 1,
                max_age_days: 1,
                deduplicate: true,
            }),
            at(3),
        )
        .unwrap();
        assert_eq!(workspace.items().len(), 1);
        assert_eq!(workspace.items()[0].item_id, pinned);

        apply(&mut workspace, WorkspaceAction::ClearUnpinned, at(3)).unwrap();
        assert_eq!(workspace.items().len(), 1);
    }

    #[test]
    fn source_exclusions_and_hashes_fail_closed() {
        let mut workspace = ClipboardWorkspace::default();
        apply(&mut workspace, WorkspaceAction::ResumeCapture, at(1)).unwrap();
        let fingerprint = format!("sha256:{}", "a".repeat(64));
        apply(
            &mut workspace,
            WorkspaceAction::SetSourceExclusions(vec![fingerprint.clone()]),
            at(1),
        )
        .unwrap();
        let mut excluded = ingest(Uuid::new_v4(), "private", at(1));
        if let WorkspaceAction::IngestText {
            source_fingerprint, ..
        } = &mut excluded
        {
            *source_fingerprint = Some(fingerprint);
        }
        assert_eq!(
            apply(&mut workspace, excluded, at(1)),
            Err(WorkspaceError::ExcludedSource)
        );

        let invalid = WorkspaceAction::IngestText {
            item_id: Uuid::new_v4(),
            text: "text".into(),
            content_sha256: "0".repeat(64),
            byte_size: 4,
            captured_at: at(1),
            source_fingerprint: None,
        };
        assert_eq!(
            apply(&mut workspace, invalid, at(1)),
            Err(WorkspaceError::InvalidContentHash)
        );
    }

    #[test]
    fn duplicate_commands_are_idempotently_rejected() {
        let mut workspace = ClipboardWorkspace::default();
        let command_id = Uuid::new_v4();
        workspace
            .apply(command_id, 0, WorkspaceAction::ResumeCapture, at(1))
            .unwrap();
        let before = workspace.clone();
        assert_eq!(
            workspace.apply(command_id, 1, WorkspaceAction::PauseCapture, at(1)),
            Err(WorkspaceError::DuplicateCommand)
        );
        assert_eq!(workspace, before);
    }

    #[test]
    fn checked_in_feature_manifest_matches_the_reducer_contract() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../contracts/desktop-feature-manifest.json"))
                .unwrap();
        assert_eq!(manifest["implementation"], "rust_desktop");

        let mut actual = manifest["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|feature| feature["feature_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        actual.sort_unstable();

        assert_eq!(actual, DESKTOP_FEATURE_IDS);
    }
}
