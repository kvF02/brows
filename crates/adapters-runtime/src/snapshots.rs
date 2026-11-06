use super::*;
use serde::{Deserialize, Serialize};
use std::{fs::OpenOptions, io::Write};

const MANIFEST: &str = "brows-runtime.json";
const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    size: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SnapshotManifest {
    schema_version: u32,
    id: String,
    provider: String,
    version: String,
    executable: String,
    installed_at_ms: u64,
    files: BTreeMap<String, FileRecord>,
}

impl SnapshotManifest {
    fn identity(&self) -> PortResult<String> {
        profile_floor::version_key(&self.version)?;
        let (family, executable, _) = provider(&self.provider)?;
        if self.executable != executable {
            return Err(invalid_data(
                "snapshot executable does not match its provider",
            ));
        }
        let bytes = serde_json::to_vec(&self.files)
            .map_err(|_| invalid_data("cannot encode runtime inventory"))?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        Ok(format!(
            "managed-{family}-{}-{}",
            self.version,
            &digest[..16]
        ))
    }

    fn build(&self, root: &Path) -> PortResult<BrowserBuild> {
        if self.schema_version != 1
            || self.id != self.identity()?
            || self.files.is_empty()
            || self.files.len() > MAX_FILES
        {
            return Err(invalid_data("invalid managed runtime manifest"));
        }
        let (_, executable, publisher) = provider(&self.provider)?;
        let record = self
            .files
            .get(executable)
            .ok_or_else(|| invalid_data("runtime inventory omits executable"))?;
        let build = BrowserBuild {
            id: BrowserBuildId::new(self.id.clone())
                .map_err(|_| invalid_data("invalid runtime id"))?,
            provider: self.provider.clone(),
            version: self.version.clone(),
            channel: BrowserChannel::Stable,
            executable_path: root.join(executable),
            sha256: record.sha256.clone(),
            publisher: publisher.to_owned(),
            trust: RuntimeTrust::Verified,
            capabilities: brows_domain::BrowserCapabilities {
                supports_language: true,
                supports_proxy: true,
                supports_webrtc_policy: true,
                supports_devtools_pipe: true,
                fingerprint_support: FingerprintSupport::Unverified,
            },
            installed_at_ms: self.installed_at_ms,
        };
        build
            .validate()
            .map_err(|_| invalid_data("invalid runtime manifest fields"))?;
        Ok(build)
    }
}

#[derive(Debug)]
pub(crate) struct SnapshotStore {
    root: PathBuf,
}

fn provider(value: &str) -> PortResult<(&'static str, &'static str, &'static str)> {
    match value {
        "Google Chrome" => Ok(("chrome", "chrome.exe", "Google LLC")),
        "Microsoft Edge" => Ok(("edge", "msedge.exe", "Microsoft Corporation")),
        _ => Err(invalid_data(
            "only admitted stable Chrome and Edge can be captured",
        )),
    }
}

fn inventory(root: &Path) -> PortResult<BTreeMap<String, FileRecord>> {
    let mut records = BTreeMap::new();
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut bytes = 0_u64;
    let mut visited = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 32 {
            return Err(invalid_data("runtime directory nesting exceeds limit"));
        }
        for entry in
            fs::read_dir(&directory).map_err(|_| invalid_data("cannot enumerate runtime files"))?
        {
            let entry = entry.map_err(|_| invalid_data("cannot inspect runtime entry"))?;
            visited += 1;
            if visited > MAX_FILES * 2 {
                return Err(invalid_data("runtime entry count exceeds budget"));
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| invalid_data("runtime path escaped root"))?;
            if relative == Path::new(MANIFEST) {
                continue;
            }
            let name = relative
                .to_str()
                .ok_or_else(|| invalid_data("runtime paths must be Unicode"))?
                .replace('\\', "/");
            if name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'))
            {
                return Err(invalid_data("invalid runtime relative path"));
            }
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| invalid_data("cannot inspect runtime file"))?;
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return Err(invalid_data("runtime links are not allowed"));
            }
            if metadata.is_dir() {
                pending.push((path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                return Err(invalid_data("runtime entries must be ordinary files"));
            }
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| invalid_data("runtime size overflow"))?;
            if bytes > MAX_BYTES || records.len() >= MAX_FILES {
                return Err(invalid_data("runtime exceeds inventory budget"));
            }
            records.insert(
                name,
                FileRecord {
                    size: metadata.len(),
                    sha256: digest_file(&path)?,
                },
            );
        }
    }
    Ok(records)
}

fn verify_inventory(root: &Path, manifest: &SnapshotManifest) -> PortResult<()> {
    let actual = inventory(root)?;
    if actual != manifest.files {
        let changed: Vec<_> = actual
            .iter()
            .filter(|(path, record)| manifest.files.get(*path) != Some(*record))
            .take(4)
            .map(|(path, _)| {
                path.chars()
                    .filter(|c| !c.is_control())
                    .take(120)
                    .collect::<String>()
            })
            .collect();
        let missing: Vec<_> = manifest
            .files
            .keys()
            .filter(|path| !actual.contains_key(*path))
            .take(4)
            .map(|path| {
                path.chars()
                    .filter(|c| !c.is_control())
                    .take(120)
                    .collect::<String>()
            })
            .collect();
        return Err(PortError::new(
            PortErrorKind::RuntimeUntrusted,
            format!(
                "managed runtime files changed; changed/added: {}; missing: {}",
                changed.join(", "),
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

fn read_manifest(root: &Path) -> PortResult<SnapshotManifest> {
    let path = root.join(MANIFEST);
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| invalid_data("runtime manifest missing"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || is_reparse_point(&metadata)
        || metadata.len() > MAX_MANIFEST
    {
        return Err(invalid_data("invalid runtime manifest file"));
    }
    let file = File::open(path).map_err(|_| invalid_data("cannot read runtime manifest"))?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_data("cannot read runtime manifest"))?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(invalid_data("runtime manifest too large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid_data("invalid runtime manifest"))
}

fn verify_publisher_and_version(build: &BrowserBuild) -> PortResult<()> {
    let (_, _, publisher) = provider(&build.provider)?;
    verify_authenticode_publisher(&build.executable_path, &[publisher])?;
    if read_file_version(&build.executable_path)? != build.version {
        return Err(PortError::new(
            PortErrorKind::RuntimeUntrusted,
            "runtime file version differs from its inventory",
        ));
    }
    verify_digest(&build.executable_path, &build.sha256)
}

/// Chromium LPAC children need read/execute access to their own binaries.
/// Apply the documented restricted-package ACE only to a verified snapshot,
/// never to application data, credentials, profiles or parent directories.
/// https://chromium.googlesource.com/chromium/src/+/main/docs/design/sandbox.md
#[cfg(windows)]
fn configure_snapshot_acl(root: &Path) -> PortResult<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let metadata = fs::symlink_metadata(root)
        .map_err(|_| invalid_data("cannot inspect snapshot ACL target"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(invalid_data(
            "snapshot ACL target must be an ordinary directory",
        ));
    }
    let system = std::env::var_os("SystemRoot")
        .ok_or_else(|| internal("Windows system root unavailable"))?;
    let mut child = Command::new(PathBuf::from(system).join("System32/icacls.exe"))
        .arg(root)
        .args(["/grant", "*S-1-15-2-2:(OI)(CI)(RX)", "/T", "/Q"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|_| internal("cannot configure snapshot sandbox read permissions"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(invalid_data(
                    "snapshot sandbox read permission setup failed",
                ))
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(internal("cannot inspect snapshot ACL setup"));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(internal("snapshot ACL setup timed out"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

#[cfg(not(windows))]
fn configure_snapshot_acl(_root: &Path) -> PortResult<()> {
    Ok(())
}

#[derive(Debug)]
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl ManagedChromiumRuntime {
    /// Discover immutable local snapshots. Corrupt entries are excluded, while
    /// issues remain visible to the management UI. No downloads or arbitrary paths.
    pub fn load_snapshots(&self, root: PathBuf) -> PortResult<Vec<String>> {
        let root = create_and_canonicalize(root, "snapshot root")?;
        self.ensure_runtime_path(&root)?;
        let mut issues = Vec::new();
        for (index, entry) in fs::read_dir(&root)
            .map_err(|_| invalid_data("cannot enumerate runtime snapshots"))?
            .enumerate()
        {
            if index >= 128 {
                issues.push("Snapshot count exceeds the 128-entry admission budget".to_owned());
                break;
            }
            let entry = entry.map_err(|_| invalid_data("cannot inspect snapshot directory"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("managed-") {
                continue;
            }
            let result = (|| {
                let metadata = fs::symlink_metadata(entry.path())
                    .map_err(|_| invalid_data("cannot inspect runtime snapshot"))?;
                if !metadata.is_dir()
                    || metadata.file_type().is_symlink()
                    || is_reparse_point(&metadata)
                {
                    return Err(invalid_data("invalid runtime directory"));
                }
                let manifest = read_manifest(&entry.path())?;
                if name != manifest.id {
                    return Err(invalid_data("runtime directory and identity differ"));
                }
                let build = manifest.build(&entry.path())?;
                verify_inventory(&entry.path(), &manifest)?;
                verify_publisher_and_version(&build)?;
                configure_snapshot_acl(&entry.path())?;
                self.snapshot_inventories
                    .write()
                    .map_err(|_| internal("runtime inventory lock unavailable"))?
                    .insert(build.id.clone(), manifest);
                self.register_build(build, false)
            })();
            if result.is_err() {
                issues.push("A managed runtime failed integrity admission and was excluded; pinned profiles remain blocked".to_owned());
            }
        }
        *self
            .snapshot_store
            .write()
            .map_err(|_| internal("runtime snapshot store unavailable"))? =
            Some(SnapshotStore { root });
        Ok(issues)
    }

    pub(crate) fn verify_snapshot(&self, build: &BrowserBuild) -> PortResult<()> {
        let inventories = self
            .snapshot_inventories
            .read()
            .map_err(|_| internal("runtime inventory unavailable"))?;
        if let Some(manifest) = inventories.get(&build.id) {
            let root = build
                .executable_path
                .parent()
                .ok_or_else(|| invalid_data("runtime root missing"))?;
            verify_inventory(root, manifest)?;
        }
        Ok(())
    }

    /// Capture only a registered installed browser. Existing versions are never
    /// overwritten. Source updates or copy failures discard the staging directory.
    pub fn capture_snapshot(&self, id: &BrowserBuildId) -> PortResult<BrowserBuild> {
        let _operation = self
            .snapshot_operations
            .lock()
            .map_err(|_| internal("runtime management unavailable"))?;
        let source = self.resolve_build(&BrowserBuildSelector::Pinned(id.clone()))?;
        if id.as_str().starts_with("managed-") {
            return Err(invalid_data("this build is already managed"));
        }
        if source.channel != BrowserChannel::Stable {
            return Err(invalid_data("only stable installations can be captured"));
        }
        verify_publisher_and_version(&source)?;
        let store = self
            .snapshot_store
            .read()
            .map_err(|_| internal("runtime snapshot store unavailable"))?;
        let store = store
            .as_ref()
            .ok_or_else(|| invalid_data("managed runtime storage is not configured"))?;
        let source_root = source
            .executable_path
            .parent()
            .ok_or_else(|| invalid_data("source runtime root missing"))?;
        let stage = Staging(store.root.join(format!(
                "staging-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )));
        fs::create_dir(&stage.0)
            .map_err(|_| internal("cannot allocate runtime staging directory"))?;
        // Select the executable, its matching native version directory and other
        // root resources; omit unrelated installed numeric versions.
        let mut pending = Vec::new();
        for entry in fs::read_dir(source_root)
            .map_err(|_| invalid_data("cannot enumerate installed browser"))?
        {
            let entry = entry.map_err(|_| invalid_data("cannot inspect installed browser"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // Installer histogram spools are consumed/deleted by the browser.
            // They are not distribution resources and must never enter a snapshot.
            if name.eq_ignore_ascii_case("SetupMetrics") {
                continue;
            }
            if profile_floor::version_key(&name).is_ok() && name != source.version {
                continue;
            }
            pending.push((entry.path(), PathBuf::from(entry.file_name()), 0));
        }
        let mut count = 0_usize;
        let mut bytes = 0_u64;
        while let Some((from, relative, depth)) = pending.pop() {
            if depth > 32 {
                return Err(invalid_data("runtime nesting exceeds copy budget"));
            }
            let metadata = fs::symlink_metadata(&from)
                .map_err(|_| invalid_data("cannot inspect runtime source"))?;
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return Err(invalid_data("runtime links cannot be captured"));
            }
            let target = stage.0.join(&relative);
            if metadata.is_dir() {
                fs::create_dir(&target)
                    .map_err(|_| internal("cannot create runtime snapshot directory"))?;
                for entry in fs::read_dir(from)
                    .map_err(|_| invalid_data("cannot enumerate runtime source"))?
                {
                    let entry = entry.map_err(|_| invalid_data("cannot inspect runtime source"))?;
                    pending.push((entry.path(), relative.join(entry.file_name()), depth + 1));
                }
                continue;
            }
            if !metadata.is_file() {
                return Err(invalid_data("runtime source is not a regular file"));
            }
            count += 1;
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| invalid_data("runtime size overflow"))?;
            if count > MAX_FILES || bytes > MAX_BYTES {
                return Err(invalid_data("runtime exceeds copy budget"));
            }
            let expected = digest_file(&from)?;
            fs::copy(&from, &target).map_err(|_| {
                internal("cannot copy runtime file; installed browser may be updating")
            })?;
            if digest_file(&from)? != expected || digest_file(&target)? != expected {
                return Err(invalid_data("installed runtime changed during capture"));
            }
        }
        verify_publisher_and_version(&source)?;
        let (_, executable, _) = provider(&source.provider)?;
        let mut manifest = SnapshotManifest {
            schema_version: 1,
            id: String::new(),
            provider: source.provider.clone(),
            version: source.version.clone(),
            executable: executable.to_owned(),
            installed_at_ms: now_ms(),
            files: inventory(&stage.0)?,
        };
        manifest.id = manifest.identity()?;
        let candidate = manifest.build(&stage.0)?;
        verify_publisher_and_version(&candidate)?;
        configure_snapshot_acl(&stage.0)?;
        let target = store.root.join(&manifest.id);
        if target.exists() {
            let metadata = fs::symlink_metadata(&target)
                .map_err(|_| invalid_data("cannot inspect existing snapshot"))?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || is_reparse_point(&metadata)
            {
                return Err(invalid_data(
                    "existing snapshot must be an ordinary directory",
                ));
            }
            let existing = read_manifest(&target)?;
            if existing.identity()? != manifest.id {
                return Err(invalid_data("existing runtime snapshot identity mismatch"));
            }
            verify_inventory(&target, &existing)?;
            return self.resolve_build(&BrowserBuildSelector::Pinned(candidate.id));
        }
        let encoded = serde_json::to_vec_pretty(&manifest)
            .map_err(|_| internal("cannot encode runtime manifest"))?;
        if encoded.len() as u64 > MAX_MANIFEST {
            return Err(invalid_data("runtime manifest exceeds size budget"));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.0.join(MANIFEST))
            .map_err(|_| internal("cannot publish runtime manifest"))?;
        file.write_all(&encoded)
            .and_then(|_| file.sync_all())
            .map_err(|_| internal("cannot persist runtime manifest"))?;
        drop(file);
        fs::rename(&stage.0, &target)
            .map_err(|_| internal("cannot atomically publish runtime snapshot"))?;
        let build = manifest.build(&target)?;
        self.snapshot_inventories
            .write()
            .map_err(|_| internal("runtime inventory lock unavailable"))?
            .insert(build.id.clone(), manifest);
        self.register_build(build.clone(), false)?;
        Ok(build)
    }

    /// Persist channel selection before publishing it, while registry readers
    /// are excluded. Management services also hold the idle session gate.
    pub fn activate_persisted(
        &self,
        id: &BrowserBuildId,
        persist: impl FnOnce() -> PortResult<()>,
    ) -> PortResult<()> {
        let build = self.resolve_build(&BrowserBuildSelector::Pinned(id.clone()))?;
        verify_digest(&build.executable_path, &build.sha256)?;
        self.verify_snapshot(&build)?;
        let mut registry = self
            .builds
            .write()
            .map_err(|_| internal("runtime registry unavailable"))?;
        persist()?;
        registry
            .active_channels
            .insert(channel_key(build.channel), id.clone());
        Ok(())
    }

    pub fn clear_active_channel(&self, channel: BrowserChannel) -> PortResult<()> {
        self.builds
            .write()
            .map_err(|_| internal("runtime registry unavailable"))?
            .active_channels
            .remove(channel_key(channel));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventories_detect_resource_tampering_additions_and_missing_files() {
        let root = std::env::temp_dir().join(format!(
            "brows-inventory-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("msedge.exe"), b"executable").unwrap();
        fs::write(root.join("resource.pak"), b"resource").unwrap();
        let mut manifest = SnapshotManifest {
            schema_version: 1,
            id: String::new(),
            provider: "Microsoft Edge".to_owned(),
            version: "156.0.1.2".to_owned(),
            executable: "msedge.exe".to_owned(),
            installed_at_ms: 1,
            files: inventory(&root).unwrap(),
        };
        manifest.id = manifest.identity().unwrap();
        assert!(manifest.build(&root).is_ok());
        verify_inventory(&root, &manifest).unwrap();
        fs::write(root.join("resource.pak"), b"modified").unwrap();
        assert!(verify_inventory(&root, &manifest).is_err());
        fs::write(root.join("resource.pak"), b"resource").unwrap();
        fs::write(root.join("unexpected.dll"), b"extra").unwrap();
        assert!(verify_inventory(&root, &manifest).is_err());
        fs::remove_file(root.join("unexpected.dll")).unwrap();
        fs::create_dir(root.join("SetupMetrics")).unwrap();
        fs::write(root.join("SetupMetrics").join("unexpected.pma"), b"extra").unwrap();
        assert!(verify_inventory(&root, &manifest).is_err());
        fs::remove_dir_all(root.join("SetupMetrics")).unwrap();
        fs::remove_file(root.join("resource.pak")).unwrap();
        assert!(verify_inventory(&root, &manifest).is_err());
        manifest.executable = "../outside.exe".to_owned();
        assert!(manifest.build(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
