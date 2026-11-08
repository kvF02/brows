#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use brows_application::{PortError, PortErrorKind, PortResult};
use brows_domain::ProfileId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub const INTENT_FILE: &str = ".brows-data-operation.json";

#[derive(Debug, Clone)]
pub struct DataRootLease {
    _file: Arc<File>,
}
impl DataRootLease {
    pub fn acquire(root: &Path) -> PortResult<Self> {
        let root = root
            .canonicalize()
            .map_err(|_| unavailable("cannot resolve application data root"))?;
        let path = root.join(".brows-data.lock");
        if path.symlink_metadata().is_ok() {
            ordinary(&path, false)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| unavailable("cannot open application data lease"))?;
        file.try_lock().map_err(|_| {
            PortError::new(
                PortErrorKind::Locked,
                "another Brows instance or data operation owns this data directory",
            )
        })?;
        Ok(Self {
            _file: Arc::new(file),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Restore,
    Migration,
    Rollback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataOperation {
    pub version: u8,
    pub kind: OperationKind,
    pub profile_id: String,
    pub operation_key: String,
}
impl DataOperation {
    pub fn new(kind: OperationKind, profile: &ProfileId, key: &str) -> PortResult<Self> {
        let value = Self {
            version: 1,
            kind,
            profile_id: profile.as_str().to_owned(),
            operation_key: key.to_owned(),
        };
        value.validate()?;
        Ok(value)
    }
    pub fn profile(&self) -> PortResult<ProfileId> {
        ProfileId::new(self.profile_id.clone())
            .map_err(|_| invalid("invalid recovery profile identity"))
    }
    pub fn migration_id(&self) -> PortResult<u64> {
        let id = self
            .operation_key
            .strip_prefix("migration-")
            .and_then(|id| id.parse::<u64>().ok())
            .filter(|id| *id > 0)
            .ok_or_else(|| invalid("invalid recovery migration identity"))?;
        if self.operation_key != format!("migration-{id}") {
            return Err(invalid("noncanonical recovery migration identity"));
        }
        Ok(id)
    }
    fn validate(&self) -> PortResult<()> {
        self.profile()?;
        if self.version != 1 {
            return Err(invalid("unsupported data-operation version"));
        }
        match self.kind {
            OperationKind::Restore if self.operation_key == "restore" => Ok(()),
            OperationKind::Migration | OperationKind::Rollback
                if self.operation_key.len() <= 40 =>
            {
                self.migration_id().map(|_| ())
            }
            _ => Err(invalid("invalid recovery operation")),
        }
    }
}

#[derive(Debug)]
pub struct ProfileDataJournal {
    root: PathBuf,
    _lease: Option<DataRootLease>,
}
impl ProfileDataJournal {
    pub fn new(root: PathBuf, lease: Option<DataRootLease>) -> PortResult<Self> {
        let root = root
            .canonicalize()
            .map_err(|_| unavailable("cannot resolve profile root"))?;
        ordinary(&root, true)?;
        Ok(Self {
            root,
            _lease: lease,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn profile_dir(&self, profile: &ProfileId) -> PathBuf {
        self.root
            .join(format!("{:x}", Sha256::digest(profile.as_str().as_bytes())))
    }
    pub fn begin(&self, operation: &DataOperation) -> PortResult<()> {
        operation.validate()?;
        let parent = self.profile_dir(&operation.profile()?);
        if !parent.exists() {
            fs::create_dir(&parent)
                .map_err(|_| unavailable("cannot create operation directory"))?;
        }
        ordinary(&parent, true)?;
        if parent.canonicalize().ok().as_ref() != Some(&parent) {
            return Err(invalid("operation directory is not confined"));
        }
        let path = parent.join(INTENT_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| {
                PortError::new(
                    PortErrorKind::Locked,
                    "profile has an unfinished data operation; restart Brows to recover it",
                )
            })?;
        let bytes =
            serde_json::to_vec(operation).map_err(|_| invalid("cannot encode data intent"))?;
        if file
            .write_all(&bytes)
            .and_then(|_| file.sync_all())
            .is_err()
        {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(unavailable("cannot persist data intent"));
        }
        sync_directory(&parent)?;
        sync_directory(&self.root)?;
        Ok(())
    }
    pub fn finish(&self, profile: &ProfileId) -> PortResult<()> {
        let parent = self.profile_dir(profile);
        if parent
            .symlink_metadata()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        ordinary(&parent, true)?;
        let path = parent.join(INTENT_FILE);
        match path.symlink_metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(unavailable("cannot inspect data intent")),
            Ok(_) => {
                ordinary(&path, false)?;
                fs::remove_file(path).map_err(|_| unavailable("cannot clear data intent"))?;
                sync_directory(&parent)
            }
        }
    }
    pub fn pending(&self) -> PortResult<Vec<DataOperation>> {
        let mut result = Vec::new();
        for (index, item) in fs::read_dir(&self.root)
            .map_err(|_| unavailable("cannot list profile data"))?
            .enumerate()
        {
            if index >= 20000 {
                return Err(invalid("profile recovery directory limit exceeded"));
            }
            let item = item.map_err(|_| unavailable("cannot read profile entry"))?;
            let name = item.file_name();
            let name = name.to_string_lossy();
            if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let parent = item.path();
            ordinary(&parent, true)?;
            let path = parent.join(INTENT_FILE);
            match path.symlink_metadata() {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(unavailable("cannot inspect recovery intent")),
                Ok(_) => {}
            }
            ordinary(&path, false)?;
            let mut bytes = Vec::new();
            File::open(&path)
                .and_then(|f| f.take(4097).read_to_end(&mut bytes))
                .map_err(|_| unavailable("cannot read recovery intent"))?;
            if bytes.len() > 4096 {
                return Err(invalid("recovery intent exceeds limit"));
            }
            let op: DataOperation = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("invalid recovery intent; data preserved"))?;
            op.validate()?;
            if self.profile_dir(&op.profile()?) != parent {
                return Err(invalid("recovery identity does not match its directory"));
            }
            result.push(op);
        }
        Ok(result)
    }
}

pub fn ensure_profile_available(user_data: &Path) -> PortResult<()> {
    let parent = user_data
        .parent()
        .ok_or_else(|| invalid("profile directory missing"))?;
    match parent.join(INTENT_FILE).symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(unavailable("cannot inspect profile recovery state")),
        Ok(_) => Err(PortError::new(
            PortErrorKind::Locked,
            "profile has an unfinished data operation; restart Brows to recover it",
        )),
    }
}

pub fn ordinary(path: &Path, directory: bool) -> PortResult<()> {
    let meta = path
        .symlink_metadata()
        .map_err(|_| unavailable("cannot inspect managed data"))?;
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let linked = meta.file_type().is_symlink();
    if linked || (directory && !meta.is_dir()) || (!directory && !meta.is_file()) {
        return Err(invalid(
            "managed data contains a link or unexpected file type",
        ));
    }
    Ok(())
}

/// Flush copied files before publishing directory swaps. This requests OS durability;
/// physical power-loss behavior still depends on the filesystem and hardware.
pub fn sync_tree(path: &Path) -> PortResult<()> {
    fn sync(path: &Path, count: &mut usize, depth: usize) -> PortResult<()> {
        *count += 1;
        if *count > 200000 || depth > 64 {
            return Err(invalid("sync tree exceeds limits"));
        }
        let directory = path
            .symlink_metadata()
            .map_err(|_| unavailable("cannot inspect data for sync"))?
            .is_dir();
        ordinary(path, directory)?;
        if directory {
            for item in fs::read_dir(path).map_err(|_| unavailable("cannot read data for sync"))? {
                sync(
                    &item
                        .map_err(|_| unavailable("cannot read sync entry"))?
                        .path(),
                    count,
                    depth + 1,
                )?;
            }
            sync_directory(path)?;
        } else {
            #[cfg(windows)]
            let file = OpenOptions::new().write(true).open(path);
            #[cfg(not(windows))]
            let file = File::open(path);
            file.and_then(|f| f.sync_all())
                .map_err(|_| unavailable("cannot flush staged profile data"))?;
        }
        Ok(())
    }
    sync(path, &mut 0, 0)
}

pub fn sync_directory(path: &Path) -> PortResult<()> {
    // Rust/Windows offers no portable directory-fsync contract. Keep the file
    // intents and copied-file flushes; do not claim hardware power-cut coverage.
    #[cfg(unix)]
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| unavailable("cannot flush data directory"))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn remove_confined_tree(path: &Path, root: &Path) -> PortResult<()> {
    if !path.starts_with(root) || path == root || root.canonicalize().ok().as_deref() != Some(root)
    {
        return Err(invalid("cleanup path is not confined"));
    }
    if path
        .symlink_metadata()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    fn inspect(path: &Path, count: &mut usize, depth: usize) -> PortResult<()> {
        *count += 1;
        if *count > 200000 || depth > 64 {
            return Err(invalid("cleanup tree exceeds limits"));
        }
        let dir = path
            .symlink_metadata()
            .map_err(|_| unavailable("cannot inspect cleanup tree"))?
            .is_dir();
        ordinary(path, dir)?;
        if dir {
            for entry in fs::read_dir(path).map_err(|_| unavailable("cannot list cleanup tree"))? {
                inspect(
                    &entry
                        .map_err(|_| unavailable("cannot read cleanup entry"))?
                        .path(),
                    count,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }
    if path.canonicalize().ok().as_deref() != Some(path) {
        return Err(invalid("cleanup path contains a link"));
    }
    inspect(path, &mut 0, 0)?;
    fs::remove_dir_all(path).map_err(|_| unavailable("cannot remove interrupted data"))
}
fn invalid(message: &str) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}
fn unavailable(message: &str) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}

#[cfg(test)]
mod tests;
