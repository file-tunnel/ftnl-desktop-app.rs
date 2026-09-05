//! Session-secret handling and fail-closed download persistence.

use std::fmt;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use ftnl_client::{FileDescriptor, Tunnel};
use tempfile::NamedTempFile;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_FILES_PER_TUNNEL: u16 = 10;
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_FILE_NAME_BYTES: usize = 255;
const MAX_MEDIA_TYPE_BYTES: usize = 128;
const MAX_CREATED_AT_BYTES: usize = 64;

/// A desktop receive session. Credentials are process-only and zeroized when
/// the session is replaced, cancelled, or the process exits.
pub struct ReceiveSession {
    pub tunnel_id: Uuid,
    pub expires_at: String,
    pairing_uri: Zeroizing<String>,
    desktop_capability: Zeroizing<String>,
}

impl ReceiveSession {
    pub fn from_tunnel(tunnel: Tunnel) -> Result<Self, SessionError> {
        validate_tunnel(&tunnel)?;
        Ok(Self {
            tunnel_id: tunnel.tunnel_id,
            expires_at: tunnel.expires_at,
            pairing_uri: Zeroizing::new(tunnel.pairing_uri),
            desktop_capability: Zeroizing::new(tunnel.desktop_capability),
        })
    }

    pub fn pairing_uri(&self) -> &str {
        self.pairing_uri.as_str()
    }

    pub fn capability(&self) -> &str {
        self.desktop_capability.as_str()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SessionError {
    #[error("server returned a tunnel outside the desktop receive contract")]
    InvalidTunnel,
}

fn validate_tunnel(tunnel: &Tunnel) -> Result<(), SessionError> {
    let uri = Url::parse(&tunnel.pairing_uri).map_err(|_| SessionError::InvalidTunnel)?;
    let secure_scheme = match uri.scheme() {
        "https" => true,
        "http" => uri.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }),
        _ => false,
    };
    let expected_id = tunnel.tunnel_id.to_string();
    let path_matches = uri
        .path_segments()
        .is_some_and(|segments| segments.collect::<Vec<_>>() == ["t", expected_id.as_str()]);
    let fragment_fields = uri
        .fragment()
        .map(|fragment| url::form_urlencoded::parse(fragment.as_bytes()).collect::<Vec<_>>())
        .unwrap_or_default();
    let pairing_secret_is_bounded = matches!(
        fragment_fields.as_slice(),
        [(key, secret)]
            if key == "c"
                && (32..=512).contains(&secret.len())
                && secret
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    );
    let capability_is_bounded = (32..=512).contains(&tunnel.desktop_capability.len())
        && tunnel
            .desktop_capability
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    let expiry_is_bounded = (1..=64).contains(&tunnel.expires_at.len())
        && !tunnel.expires_at.chars().any(char::is_control);
    if tunnel.tunnel_id.is_nil()
        || tunnel.status != "waiting"
        || tunnel.pairing_uri.len() > 2_048
        || tunnel.pairing_uri.chars().any(char::is_control)
        || !secure_scheme
        || uri.host().is_none()
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.query().is_some()
        || !path_matches
        || !pairing_secret_is_bounded
        || !capability_is_bounded
        || !expiry_is_bounded
    {
        return Err(SessionError::InvalidTunnel);
    }
    Ok(())
}

impl fmt::Debug for ReceiveSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceiveSession")
            .field("tunnel_id", &self.tunnel_id)
            .field("expires_at", &self.expires_at)
            .field("pairing_uri", &"[REDACTED]")
            .field("desktop_capability", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("server returned an unsafe file name")]
    UnsafeFileName,
    #[error("file metadata is outside the desktop receive contract")]
    InvalidMetadata,
    #[error("downloaded byte count did not match the declaration")]
    SizeMismatch,
    #[error("output directory does not exist")]
    MissingOutputDirectory,
    #[error("cannot persist the downloaded file: {0}")]
    Io(#[source] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DescriptorError {
    #[error("snapshot contains too many files")]
    TooManyFiles,
    #[error("snapshot contains duplicate file identifiers")]
    DuplicateFileId,
    #[error("file metadata is outside the desktop receive contract")]
    InvalidMetadata,
}

pub fn validate_snapshot(files: &[FileDescriptor]) -> Result<(), DescriptorError> {
    if files.len() > usize::from(MAX_FILES_PER_TUNNEL) {
        return Err(DescriptorError::TooManyFiles);
    }
    let mut identifiers = std::collections::HashSet::with_capacity(files.len());
    for file in files {
        validate_file_descriptor(file)?;
        if !identifiers.insert(file.file_id) {
            return Err(DescriptorError::DuplicateFileId);
        }
    }
    Ok(())
}

pub fn validate_file_descriptor(file: &FileDescriptor) -> Result<(), DescriptorError> {
    let valid_file_id = !file.file_id.is_nil();
    let valid_name = !file.name.is_empty()
        && file.name.len() <= MAX_FILE_NAME_BYTES
        && !file.name.chars().any(char::is_control)
        && safe_default_destination(&file.name).is_ok();
    let valid_media_type = !file.media_type.is_empty()
        && file.media_type.len() <= MAX_MEDIA_TYPE_BYTES
        && file
            .media_type
            .chars()
            .all(|character| character.is_ascii_graphic());
    let valid_status = matches!(
        file.status.as_str(),
        "declared" | "uploading" | "available" | "downloaded" | "rejected" | "cancelled"
    );
    let valid_sizes =
        file.size_bytes <= MAX_FILE_BYTES && file.bytes_transferred <= file.size_bytes;
    let valid_created_at = (1..=MAX_CREATED_AT_BYTES).contains(&file.created_at.len())
        && !file.created_at.chars().any(char::is_control);
    if valid_file_id
        && valid_name
        && valid_media_type
        && valid_status
        && valid_sizes
        && valid_created_at
    {
        Ok(())
    } else {
        Err(DescriptorError::InvalidMetadata)
    }
}

pub fn safe_default_destination(name: &str) -> Result<PathBuf, SaveError> {
    let has_portable_path_punctuation = name
        .chars()
        .any(|character| matches!(character, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    let has_windows_trailing_separator = matches!(name.chars().last(), Some(' ' | '.'));
    let windows_stem = name.split('.').next().unwrap_or(name);
    let is_reserved_windows_name = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .iter()
    .any(|reserved| windows_stem.eq_ignore_ascii_case(reserved));
    if name.is_empty()
        || name.len() > MAX_FILE_NAME_BYTES
        || name.chars().any(char::is_control)
        || has_portable_path_punctuation
        || has_windows_trailing_separator
        || is_reserved_windows_name
    {
        return Err(SaveError::UnsafeFileName);
    }
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if !component.is_empty() => {
            Ok(PathBuf::from(component))
        }
        _ => Err(SaveError::UnsafeFileName),
    }
}

pub fn save_download(
    file: &FileDescriptor,
    bytes: &[u8],
    requested_destination: Option<&Path>,
    force: bool,
) -> Result<PathBuf, SaveError> {
    validate_file_descriptor(file).map_err(|_| SaveError::InvalidMetadata)?;
    let received = u64::try_from(bytes.len()).map_err(|_| SaveError::SizeMismatch)?;
    if received != file.size_bytes {
        return Err(SaveError::SizeMismatch);
    }
    let destination = requested_destination
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(|| safe_default_destination(&file.name))?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(SaveError::MissingOutputDirectory);
    }

    let mut temporary = NamedTempFile::new_in(parent).map_err(SaveError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(SaveError::Io)?;
    }
    temporary.write_all(bytes).map_err(SaveError::Io)?;
    temporary.as_file().sync_all().map_err(SaveError::Io)?;

    let persisted = if force {
        temporary.persist(&destination)
    } else {
        temporary.persist_noclobber(&destination)
    };
    persisted.map_err(|error| SaveError::Io(error.error))?;

    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(SaveError::Io)?;

    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: &str, size_bytes: u64) -> FileDescriptor {
        FileDescriptor {
            file_id: Uuid::from_u128(1),
            name: name.into(),
            media_type: "application/octet-stream".into(),
            size_bytes,
            bytes_transferred: size_bytes,
            status: "available".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn session_debug_redacts_both_credentials() {
        let tunnel_id = Uuid::from_u128(1);
        let session = ReceiveSession::from_tunnel(Tunnel {
            tunnel_id,
            pairing_uri: format!(
                "https://upload.file-tunnel.dev/t/{tunnel_id}#c={}",
                "p".repeat(64)
            ),
            desktop_capability: "d".repeat(64),
            expires_at: "2026-01-01T00:00:00Z".into(),
            status: "waiting".into(),
        })
        .unwrap();
        let rendered = format!("{session:?}");
        assert!(!rendered.contains(&"p".repeat(64)));
        assert!(!rendered.contains(&"d".repeat(64)));
        assert_eq!(rendered.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn tunnel_contract_rejects_credential_smuggling_and_wrong_routes() {
        let tunnel_id = Uuid::from_u128(1);
        let valid = || Tunnel {
            tunnel_id,
            pairing_uri: format!(
                "https://upload.file-tunnel.dev/t/{tunnel_id}#c={}",
                "p".repeat(64)
            ),
            desktop_capability: "d".repeat(64),
            expires_at: "2026-01-01T00:00:00Z".into(),
            status: "waiting".into(),
        };
        assert!(ReceiveSession::from_tunnel(valid()).is_ok());

        for pairing_uri in [
            format!(
                "http://upload.file-tunnel.dev/t/{tunnel_id}#c={}",
                "p".repeat(64)
            ),
            format!(
                "https://user@upload.file-tunnel.dev/t/{tunnel_id}#c={}",
                "p".repeat(64)
            ),
            format!(
                "https://upload.file-tunnel.dev/t/{tunnel_id}?c={}",
                "p".repeat(64)
            ),
            format!(
                "https://upload.file-tunnel.dev/wrong/{tunnel_id}#c={}",
                "p".repeat(64)
            ),
            format!(
                "https://upload.file-tunnel.dev/t/{tunnel_id}#c={}&extra=value",
                "p".repeat(64)
            ),
        ] {
            let mut tunnel = valid();
            tunnel.pairing_uri = pairing_uri;
            assert!(ReceiveSession::from_tunnel(tunnel).is_err());
        }
    }

    #[test]
    fn default_name_rejects_every_path_form() {
        assert_eq!(
            safe_default_destination("photo.jpg").unwrap(),
            PathBuf::from("photo.jpg")
        );
        for unsafe_name in [
            "../photo.jpg",
            "folder/photo.jpg",
            "folder\\photo.jpg",
            "/tmp/photo.jpg",
            "photo.jpg:stream",
            "photo*.jpg",
            "CON.txt",
            "trailing.",
            "line\nbreak.jpg",
            "",
        ] {
            assert!(safe_default_destination(unsafe_name).is_err());
        }
        assert!(safe_default_destination(&"a".repeat(256)).is_err());
    }

    #[test]
    fn snapshot_metadata_is_closed_bounded_and_internally_consistent() {
        let valid = descriptor("photo.jpg", 3);
        assert_eq!(validate_snapshot(std::slice::from_ref(&valid)), Ok(()));

        let mut oversized = valid.clone();
        oversized.size_bytes = MAX_FILE_BYTES + 1;
        assert_eq!(
            validate_file_descriptor(&oversized),
            Err(DescriptorError::InvalidMetadata)
        );

        let mut impossible_progress = valid.clone();
        impossible_progress.bytes_transferred = impossible_progress.size_bytes + 1;
        assert_eq!(
            validate_file_descriptor(&impossible_progress),
            Err(DescriptorError::InvalidMetadata)
        );

        let mut unknown_status = valid.clone();
        unknown_status.status = "future_status".into();
        assert_eq!(
            validate_file_descriptor(&unknown_status),
            Err(DescriptorError::InvalidMetadata)
        );

        assert_eq!(
            validate_snapshot(&[valid.clone(), valid.clone()]),
            Err(DescriptorError::DuplicateFileId)
        );
        assert_eq!(
            validate_snapshot(&vec![
                descriptor("photo.jpg", 3);
                usize::from(MAX_FILES_PER_TUNNEL) + 1
            ]),
            Err(DescriptorError::TooManyFiles)
        );

        let mut nil_id = valid.clone();
        nil_id.file_id = Uuid::nil();
        assert_eq!(
            validate_file_descriptor(&nil_id),
            Err(DescriptorError::InvalidMetadata)
        );

        let mut invalid_created_at = valid.clone();
        invalid_created_at.created_at = "2026-01-01\n".into();
        assert_eq!(
            validate_file_descriptor(&invalid_created_at),
            Err(DescriptorError::InvalidMetadata)
        );
    }

    #[test]
    fn download_is_size_checked_atomic_and_no_clobber() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("photo.jpg");
        let file = descriptor("photo.jpg", 3);
        assert!(save_download(&file, b"no", Some(&destination), false).is_err());
        save_download(&file, b"one", Some(&destination), false).unwrap();
        assert!(save_download(&file, b"two", Some(&destination), false).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"one");
        save_download(&file, b"two", Some(&destination), true).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"two");
    }

    #[test]
    fn download_rechecks_descriptor_metadata_at_persistence_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("photo.jpg");
        let mut invalid = descriptor("photo.jpg", 3);
        invalid.file_id = Uuid::nil();

        assert!(matches!(
            save_download(&invalid, b"one", Some(&destination), false),
            Err(SaveError::InvalidMetadata)
        ));
        assert!(!destination.exists());
    }
}
