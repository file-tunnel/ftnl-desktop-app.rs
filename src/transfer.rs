//! Session-secret handling and fail-closed download persistence.

use std::fmt;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use ftnl_client::{FileDescriptor, Tunnel};
use tempfile::NamedTempFile;
use uuid::Uuid;
use zeroize::Zeroizing;

/// A desktop receive session. Credentials are process-only and zeroized when
/// the session is replaced, cancelled, or the process exits.
pub struct ReceiveSession {
    pub tunnel_id: Uuid,
    pub expires_at: String,
    pairing_uri: Zeroizing<String>,
    desktop_capability: Zeroizing<String>,
}

impl ReceiveSession {
    pub fn from_tunnel(tunnel: Tunnel) -> Self {
        Self {
            tunnel_id: tunnel.tunnel_id,
            expires_at: tunnel.expires_at,
            pairing_uri: Zeroizing::new(tunnel.pairing_uri),
            desktop_capability: Zeroizing::new(tunnel.desktop_capability),
        }
    }

    pub fn pairing_uri(&self) -> &str {
        self.pairing_uri.as_str()
    }

    pub fn capability(&self) -> &str {
        self.desktop_capability.as_str()
    }
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
    #[error("downloaded byte count did not match the declaration")]
    SizeMismatch,
    #[error("output directory does not exist")]
    MissingOutputDirectory,
    #[error("cannot persist the downloaded file: {0}")]
    Io(#[source] std::io::Error),
}

pub fn safe_default_destination(name: &str) -> Result<PathBuf, SaveError> {
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
            file_id: Uuid::nil(),
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
        let session = ReceiveSession::from_tunnel(Tunnel {
            tunnel_id: Uuid::nil(),
            pairing_uri: "ftnl://pair#c=pairing-secret".into(),
            desktop_capability: "desktop-secret".into(),
            expires_at: "2026-01-01T00:00:00Z".into(),
            status: "waiting".into(),
        });
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("pairing-secret"));
        assert!(!rendered.contains("desktop-secret"));
        assert_eq!(rendered.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn default_name_rejects_every_path_form() {
        assert_eq!(
            safe_default_destination("photo.jpg").unwrap(),
            PathBuf::from("photo.jpg")
        );
        for unsafe_name in ["../photo.jpg", "folder/photo.jpg", "/tmp/photo.jpg", ""] {
            assert!(safe_default_destination(unsafe_name).is_err());
        }
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
}
