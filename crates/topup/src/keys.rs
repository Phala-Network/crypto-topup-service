//! Key and credential files derived from dstack for containers without the dstack socket.
//!
//! Every file is published atomically with mode `0600`; the database passwords are the lowercase
//! hex of the `db/owner/v1` and `db/app/v1` keys, so every CVM of one application derives the same
//! credentials. The backup key, owner credentials, and application credentials go to three
//! directories (separate tmpfs volumes), so each consumer mounts only its own. The files are never
//! returned in an error or written to stdout/stderr.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use topup_core::SecretKey32;
use uuid::Uuid;
use zeroize::Zeroizing;

/// WAL-G key, read through `WALG_LIBSODIUM_KEY_PATH`.
pub const BACKUP_KEY_FILE: &str = "backup.key";
/// Owner password, read through the PostgreSQL image's `POSTGRES_PASSWORD_FILE`.
pub const OWNER_PASSWORD_FILE: &str = "postgres.password";
/// libpq password file of the owner login.
pub const OWNER_PGPASS_FILE: &str = "postgres.pgpass";
/// libpq password file of the application login; the init script also reads its password field.
pub const APP_PGPASS_FILE: &str = "topup_service.pgpass";

/// Writes the WAL-G libsodium key (hex).
pub fn write_backup_key(path: &Path, key: &SecretKey32) -> io::Result<()> {
    write_secret(path, &hex(key))
}

/// Writes the owner password and pgpass file into `owner_dir` and the application login's pgpass
/// file into `app_dir`.
pub fn write_database_credentials(
    owner_dir: &Path,
    app_dir: &Path,
    owner: &SecretKey32,
    app: &SecretKey32,
) -> io::Result<()> {
    let owner = hex(owner);
    write_secret(
        &app_dir.join(APP_PGPASS_FILE),
        &pgpass("topup_service", &hex(app)),
    )?;
    write_secret(
        &owner_dir.join(OWNER_PGPASS_FILE),
        &pgpass("postgres", &owner),
    )?;
    write_secret(&owner_dir.join(OWNER_PASSWORD_FILE), &owner)
}

/// Checks that every file was atomically published with a non-empty body and owner-only mode.
pub fn check(backup_dir: &Path, owner_dir: &Path, app_dir: &Path) -> io::Result<()> {
    for path in [
        backup_dir.join(BACKUP_KEY_FILE),
        app_dir.join(APP_PGPASS_FILE),
        owner_dir.join(OWNER_PGPASS_FILE),
        owner_dir.join(OWNER_PASSWORD_FILE),
    ] {
        let metadata = fs::metadata(path)?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "key file metadata is invalid",
            ));
        }
    }
    Ok(())
}

fn hex(key: &SecretKey32) -> Zeroizing<String> {
    Zeroizing::new(hex::encode(key.expose_secret()))
}

/// A libpq password file line matching every host, port, and database for one role.
fn pgpass(role: &str, password: &str) -> Zeroizing<String> {
    Zeroizing::new(format!("*:*:*:{role}:{password}\n"))
}

fn write_secret(path: &Path, contents: &str) -> io::Result<()> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "key path has no file name"))?
        .to_os_string();
    name.push(format!(".{}.tmp", Uuid::new_v4().simple()));
    let temporary = path.with_file_name(name);
    let result = write_temporary(&temporary, contents).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_temporary(path: &Path, contents: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn writes_hex_keys_and_pgpass_files_with_owner_only_permissions() {
        let root = std::env::temp_dir().join(format!("topup-keys-{}", Uuid::new_v4()));
        let [backup, owner_dir, app_dir] = ["backup", "owner", "app"].map(|name| root.join(name));
        for dir in [&backup, &owner_dir, &app_dir] {
            fs::create_dir_all(dir).expect("temporary directory should be created");
        }
        let owner = SecretKey32::new([0xab; 32]);
        let app = SecretKey32::new([0xcd; 32]);

        assert!(check(&backup, &owner_dir, &app_dir).is_err());
        write_backup_key(&backup.join(BACKUP_KEY_FILE), &SecretKey32::new([0xef; 32]))
            .expect("backup key should be written");
        write_database_credentials(&owner_dir, &app_dir, &owner, &app)
            .expect("credentials should be written");
        // A restarted holder replaces the files in place.
        write_database_credentials(&owner_dir, &app_dir, &owner, &app)
            .expect("credentials should be replaced");

        let read = |path: PathBuf| fs::read_to_string(path).expect("file should be readable");
        assert_eq!(read(backup.join(BACKUP_KEY_FILE)), "ef".repeat(32));
        assert_eq!(read(owner_dir.join(OWNER_PASSWORD_FILE)), "ab".repeat(32));
        assert_eq!(
            read(owner_dir.join(OWNER_PGPASS_FILE)),
            format!("*:*:*:postgres:{}\n", "ab".repeat(32))
        );
        assert_eq!(
            read(app_dir.join(APP_PGPASS_FILE)),
            format!("*:*:*:topup_service:{}\n", "cd".repeat(32))
        );
        check(&backup, &owner_dir, &app_dir).expect("published files should pass the check");
        // The application directory holds only its own login, and no temporary file remains.
        let names = |dir: &Path| {
            let mut names = fs::read_dir(dir)
                .expect("directory should be readable")
                .map(|entry| entry.expect("entry should be readable").file_name())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        assert_eq!(names(&backup), [BACKUP_KEY_FILE]);
        assert_eq!(names(&app_dir), [APP_PGPASS_FILE]);
        assert_eq!(names(&owner_dir), [OWNER_PASSWORD_FILE, OWNER_PGPASS_FILE]);
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }
}
