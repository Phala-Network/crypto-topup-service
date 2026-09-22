//! Backup encryption key materialization.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use topup_core::SecretKey32;
use uuid::Uuid;

/// Writes a WAL-G libsodium key as hexadecimal using an atomic rename and mode `0600`.
///
/// The key bytes are never returned in an error or written to stdout/stderr.
pub fn write_libsodium_key(path: &Path, key: &SecretKey32) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "backup key path has no parent")
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "backup key path has no file name",
        )
    })?;
    let temporary = temporary_path(parent, file_name);
    let result = write_temporary(&temporary, key).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Checks that the atomically published key file has the expected size and owner-only mode.
pub fn check_libsodium_key(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() != 64 || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "backup key file metadata is invalid",
        ));
    }
    Ok(())
}

/// Returns the sibling key path used for one retained backup key version.
#[must_use]
pub fn versioned_key_path(path: &Path, version: u32) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("backup-v{version}.key"))
}

fn temporary_path(parent: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let mut name = file_name.to_os_string();
    name.push(format!(".{}.tmp", Uuid::new_v4().simple()));
    parent.join(name)
}

fn write_temporary(path: &Path, key: &SecretKey32) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    let encoded = zeroize::Zeroizing::new(hex::encode(key.expose_secret()));
    file.write_all(encoded.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_hex_key_with_owner_only_permissions() {
        let directory = std::env::temp_dir().join(format!("topup-backup-{}", Uuid::new_v4()));
        fs::create_dir(&directory).expect("temporary directory should be created");
        let path = directory.join("backup.key");

        write_libsodium_key(&path, &SecretKey32::new([0xab; 32]))
            .expect("key file should be written");

        let value = fs::read_to_string(&path).expect("key file should be readable");
        assert_eq!(value, "ab".repeat(32));
        let mode = fs::metadata(&path)
            .expect("metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        check_libsodium_key(&path).expect("key metadata should pass readiness check");
        assert_eq!(
            versioned_key_path(&path, 7),
            directory.join("backup-v7.key")
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removed");
    }
}
