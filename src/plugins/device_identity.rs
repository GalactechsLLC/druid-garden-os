use rand::RngCore;
use sha2::{Digest, Sha256};
use std::fs::{DirBuilder, OpenOptions};
use std::io::{Error, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

pub(crate) const IDENTITY_PATH: &str = "/var/lib/druid-garden/device-identity";

// Intentionally no Debug or Serialize implementation for this credential.
pub(crate) struct DeviceIdentity([u8; 32]);
impl DeviceIdentity {
    pub(crate) fn device_id(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"druid-garden-device-v2\0");
        hash.update(self.0);
        format!("dg2_{:x}", hash.finalize())
    }
    pub(crate) fn proof(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

pub(crate) fn load_or_create(path: &Path) -> Result<DeviceIdentity, Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::other("Identity parent missing"))?;
    match DirBuilder::new().mode(0o700).create(parent) {
        Ok(()) => (),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    let directory = std::fs::symlink_metadata(parent)?;
    let uid = unsafe { libc::geteuid() };
    if !directory.is_dir() || directory.uid() != uid || directory.mode() & 0o077 != 0 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Device identity directory must be private and owned by the service user",
        ));
    }
    if !path.try_exists()? {
        let mut secret = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let mut stage = tempfile::NamedTempFile::new_in(parent)?;
        stage.write_all(&secret)?;
        stage.as_file().sync_all()?;
        match stage.persist_noclobber(path) {
            Ok(_) => std::fs::File::open(parent)?.sync_all()?,
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.error),
        }
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || metadata.len() != 32
    {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Invalid or non-private device identity file",
        ));
    }
    let mut secret = [0u8; 32];
    file.read_exact(&mut secret)?;
    Ok(DeviceIdentity(secret))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn identity_protocol_digest_vector() {
        let identity = DeviceIdentity([7; 32]);
        assert_eq!(identity.proof(), "07".repeat(32));
        assert_eq!(
            identity.device_id(),
            "dg2_602047fb62c0daa051f490c55c0e9bb6e12734f865f293bc97c4f7d9b0091e55"
        );
    }
    #[test]
    fn private_identity_is_stable_and_not_machine_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private").join("identity");
        let first = load_or_create(&path).unwrap();
        let second = load_or_create(&path).unwrap();
        assert_eq!(first.device_id(), second.device_id());
        assert!(first.device_id().starts_with("dg2_"));
        assert_eq!(first.proof().len(), 64);
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create(&path).is_err());
    }
    #[test]
    fn identity_rejects_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, [1u8; 32]).unwrap();
        let path = directory.path().join("identity");
        symlink(target, &path).unwrap();
        assert!(load_or_create(&path).is_err());
    }
}
