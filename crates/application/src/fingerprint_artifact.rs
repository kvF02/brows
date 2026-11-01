//! Immutable extension directories: a private staging tree is renamed only
//! after all files are written and verified. Readers never complete old trees.
use crate::{sha256::digest_hex, stealth_template, PortError, PortErrorKind, PortResult};
use brows_domain::ProfileId;
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const OWNER: &str = ".brows-fingerprint-owner.json";
const FILES: [&str; 3] = ["manifest.json", "content.js", "background.js"];
const FILE_LIMIT: u64 = 2 * 1024 * 1024;
const STAGE_LIMIT: usize = 64;
static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

fn invalid() -> PortError {
    PortError::new(
        PortErrorKind::InvalidData,
        "指纹扩展目录、文件或身份校验失败；已保留现有内容，不会修补或覆盖",
    )
}
fn io_error() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "无法写入、发布或清理指纹扩展；未返回不完整产物，请检查存储后重试",
    )
}
fn metadata(path: &Path) -> PortResult<fs::Metadata> {
    let value = fs::symlink_metadata(path).map_err(|_| invalid())?;
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        value.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let linked = value.file_type().is_symlink();
    if linked {
        return Err(invalid());
    }
    Ok(value)
}
fn exists(path: &Path) -> PortResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(invalid()),
    }
}
// Check every existing component, including root/profile/staging ancestors.
// This is conservative path validation, not an OS handle-relative sandbox.
fn directory(path: &Path, create: bool) -> PortResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().map_err(|_| invalid())?.join(path)
    };
    if !absolute.is_absolute() {
        return Err(invalid());
    }
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) => {
                current.push(component.as_os_str());
                continue;
            }
            Component::ParentDir => {
                if !current.pop() {
                    return Err(invalid());
                }
                continue;
            }
            Component::CurDir => continue,
            _ => current.push(component.as_os_str()),
        }
        if create && !exists(&current)? {
            match fs::create_dir(&current) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(io_error()),
            }
        }
        if !metadata(&current)?.is_dir() {
            return Err(invalid());
        }
    }
    Ok(current)
}
fn read_file(path: &Path, limit: u64) -> PortResult<Vec<u8>> {
    let value = metadata(path)?;
    if !value.is_file() || value.len() > limit {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| invalid())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > limit || bytes.len() as u64 != value.len() {
        return Err(invalid());
    }
    Ok(bytes)
}
fn content_hash(files: &[Vec<u8>; 3], user_agent: &str) -> String {
    let mut bytes = Vec::new();
    for file in files {
        bytes.extend_from_slice(file);
        bytes.push(0);
    }
    bytes.extend_from_slice(user_agent.as_bytes());
    digest_hex(&bytes)
}
fn hash_name(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn names(path: &Path) -> PortResult<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).map_err(|_| invalid())? {
        let entry = entry.map_err(|_| invalid())?;
        let name = entry.file_name().into_string().map_err(|_| invalid())?;
        if names.len() >= 4 || (!FILES.contains(&name.as_str()) && name != OWNER) {
            return Err(invalid());
        }
        if !metadata(&entry.path())?.is_file() {
            return Err(invalid());
        }
        names.insert(name);
    }
    Ok(names)
}
fn owner(
    path: &Path,
    profile: &str,
    artifact: Option<&str>,
    stage: Option<&str>,
) -> PortResult<()> {
    let value: serde_json::Value =
        serde_json::from_slice(&read_file(&path.join(OWNER), 2048)?).map_err(|_| invalid())?;
    let object = value
        .as_object()
        .filter(|v| v.len() == 4)
        .ok_or_else(invalid)?;
    let id = object
        .get("stageId")
        .and_then(|v| v.as_str())
        .ok_or_else(invalid)?;
    let hash = object
        .get("artifactHash")
        .and_then(|v| v.as_str())
        .filter(|v| hash_name(v))
        .ok_or_else(invalid)?;
    if object.get("schemaVersion").and_then(|v| v.as_u64()) != Some(1)
        || object.get("profileHash").and_then(|v| v.as_str()) != Some(profile)
        || !id.starts_with("stage-")
        || id.len() > 96
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || artifact.is_some_and(|expected| expected != hash)
        || stage.is_some_and(|expected| expected != id)
    {
        return Err(invalid());
    }
    Ok(())
}

/// Verify the compiler-selected immutable path immediately before launch.
/// The hash in the selected path binds all three files and the planned UA;
/// the parent hash and exact manifest bind it to the selected profile.
pub fn verify_fingerprint_extension(
    profile: &ProfileId,
    path: &Path,
    user_agent: &str,
) -> PortResult<()> {
    if path.to_string_lossy().contains(',') {
        return Err(invalid());
    }
    let path = directory(path, false)?;
    let artifact = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| hash_name(s))
        .ok_or_else(invalid)?;
    let profile_hash = digest_hex(profile.as_str().as_bytes());
    if path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        != Some(&profile_hash)
    {
        return Err(invalid());
    }
    let entries = names(&path)?;
    if FILES.iter().any(|name| !entries.contains(*name)) {
        return Err(invalid());
    }
    if entries.contains(OWNER) {
        owner(&path, &profile_hash, Some(artifact), None)?;
    }
    let files = [
        read_file(&path.join(FILES[0]), FILE_LIMIT)?,
        read_file(&path.join(FILES[1]), FILE_LIMIT)?,
        read_file(&path.join(FILES[2]), FILE_LIMIT)?,
    ];
    if files[0] != stealth_template::render_manifest(profile.as_str()).as_bytes()
        || files[2] != stealth_template::render_background().as_bytes()
        || content_hash(&files, user_agent) != artifact
    {
        return Err(invalid());
    }
    Ok(())
}

fn lock(profile: &Path, timeout: Duration) -> PortResult<File> {
    let path = profile.join(".publish.lock");
    if exists(&path)? && !metadata(&path)?.is_file() {
        return Err(invalid());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|_| io_error())?;
    if !metadata(&path)?.is_file() {
        return Err(invalid());
    }
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(TryLockError::WouldBlock) => {
                return Err(PortError::new(
                    PortErrorKind::Unavailable,
                    "同一环境的指纹扩展正在发布，请稍后重试",
                ))
            }
            Err(_) => return Err(io_error()),
        }
    }
}
fn write_new(path: &Path, content: &[u8]) -> PortResult<()> {
    if content.len() as u64 > FILE_LIMIT {
        return Err(invalid());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| io_error())?;
    file.write_all(content)
        .and_then(|()| file.sync_all())
        .map_err(|_| io_error())
}
fn cleanup_stage(path: &Path, profile: &str) -> PortResult<()> {
    if !exists(path)? {
        return Ok(());
    }
    directory(path, false)?;
    let stage = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(invalid)?;
    owner(path, profile, None, Some(stage))?;
    let entries = names(path)?;
    // Verify the entire layout before deleting anything; leave owner until last.
    for name in &entries {
        let _ = read_file(
            &path.join(name),
            if name == OWNER { 2048 } else { FILE_LIMIT },
        )?;
    }
    for name in entries.iter().filter(|name| name.as_str() != OWNER) {
        fs::remove_file(path.join(name)).map_err(|_| io_error())?;
    }
    fs::remove_file(path.join(OWNER)).map_err(|_| io_error())?;
    fs::remove_dir(path).map_err(|_| io_error())
}
fn recover(staging: &Path, profile: &str) -> PortResult<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(staging).map_err(|_| io_error())? {
        if entries.len() >= STAGE_LIMIT {
            return Err(io_error());
        }
        entries.push(entry.map_err(|_| io_error())?.path());
    }
    for path in entries {
        // Unknown, unmarked and linked entries are retained. Only a complete
        // owner record in this profile's private staging namespace authorizes cleanup.
        if directory(&path, false).is_err() {
            continue;
        }
        let Some(stage) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if owner(&path, profile, None, Some(stage)).is_err() || names(&path).is_err() {
            continue;
        }
        cleanup_stage(&path, profile)?;
    }
    Ok(())
}
#[derive(Debug)]
struct Stage {
    path: PathBuf,
    profile: String,
}
impl Stage {
    fn create(staging: &Path, profile: &str, artifact: &str) -> PortResult<Self> {
        for _ in 0..16 {
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| io_error())?
                .as_nanos();
            let id = format!(
                "stage-{}-{time}-{}",
                std::process::id(),
                NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
            );
            let path = staging.join(&id);
            match fs::create_dir(&path) {
                Ok(()) => {
                    let stage = Self {
                        path,
                        profile: profile.into(),
                    };
                    let marker=serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"profileHash":profile,"artifactHash":artifact,"stageId":id})).map_err(|_| invalid())?;
                    write_new(&stage.path.join(OWNER), &marker)?;
                    return Ok(stage);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(io_error()),
            }
        }
        Err(io_error())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = cleanup_stage(&self.path, &self.profile);
    }
}

pub(crate) fn publish_extension(
    root: &Path,
    profile: &ProfileId,
    manifest: &str,
    content: &str,
    background: &str,
    user_agent: &str,
) -> PortResult<PathBuf> {
    if root.to_string_lossy().contains(',') {
        return Err(invalid());
    }
    if manifest != stealth_template::render_manifest(profile.as_str())
        || background != stealth_template::render_background()
        || [manifest, content, background]
            .iter()
            .any(|text| text.len() as u64 > FILE_LIMIT)
    {
        return Err(invalid());
    }
    let root = directory(root, true)?;
    if root.to_string_lossy().contains(',') {
        return Err(invalid());
    }
    let profile_hash = digest_hex(profile.as_str().as_bytes());
    let profile_dir = directory(&root.join(&profile_hash), true)?;
    let _lock = lock(&profile_dir, Duration::from_secs(3))?;
    let staging = directory(&profile_dir.join(".staging"), true)?;
    recover(&staging, &profile_hash)?;
    let files = [
        manifest.as_bytes().to_vec(),
        content.as_bytes().to_vec(),
        background.as_bytes().to_vec(),
    ];
    let artifact = content_hash(&files, user_agent);
    let target = profile_dir.join(&artifact);
    if exists(&target)? {
        verify_fingerprint_extension(profile, &target, user_agent)?;
        return Ok(target);
    }
    let stage = Stage::create(&staging, &profile_hash, &artifact)?;
    let result = (|| {
        for (name, bytes) in FILES.iter().zip(&files) {
            write_new(&stage.path.join(name), bytes)?;
        }
        // Compare complete staged bytes before the single directory rename.
        owner(&stage.path, &profile_hash, Some(&artifact), None)?;
        if names(&stage.path)?.len() != 4 {
            return Err(invalid());
        }
        for (name, bytes) in FILES.iter().zip(&files) {
            if &read_file(&stage.path.join(name), FILE_LIMIT)? != bytes {
                return Err(invalid());
            }
        }
        if exists(&target)? {
            verify_fingerprint_extension(profile, &target, user_agent)?;
            return Ok(target.clone());
        }
        fs::rename(&stage.path, &target).map_err(|_| io_error())?;
        verify_fingerprint_extension(profile, &target, user_agent)?;
        Ok(target)
    })();
    cleanup_stage(&stage.path, &profile_hash)?;
    result
}

#[cfg(test)]
mod tests;
