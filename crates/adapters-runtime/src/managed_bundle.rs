//! Immutable, inventoried Chrome for Testing installations, published by the
//! explicit installer. A manifest records integrity, never publisher trust.
use super::{canonical_regular_file, digest_file, internal, invalid_data, is_reparse_point};
use brows_application::PortResult;
use brows_domain::{BrowserBuild, BrowserBuildId, BrowserCapabilities, BrowserChannel, FingerprintSupport, RuntimeTrust};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::{Component, Path, PathBuf}};

const MANIFEST: &str = "brows-runtime.json";
const MAX_MANIFEST: u64 = 2 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct Bundle {
    root: PathBuf,
    files: BTreeMap<PathBuf, String>,
    version: String,
}

impl Bundle {
    pub(super) fn load(executable: &Path) -> PortResult<Self> {
        let root = executable.parent().and_then(Path::parent)
            .ok_or_else(|| invalid_data("invalid managed runtime layout"))?.canonicalize()
            .map_err(|_| invalid_data("managed runtime root is missing"))?;
        let manifest = canonical_regular_file(&root.join(MANIFEST), "runtime manifest")?;
        if !manifest.starts_with(&root) || fs::metadata(&manifest).map_err(|_| internal("cannot inspect runtime manifest"))?.len() > MAX_MANIFEST {
            return Err(invalid_data("invalid runtime manifest"));
        }
        let value: Value = serde_json::from_slice(&fs::read(&manifest).map_err(|_| internal("cannot read runtime manifest"))?)
            .map_err(|_| invalid_data("invalid runtime manifest JSON"))?;
        let version = value["version"].as_str().ok_or_else(|| invalid_data("missing runtime version"))?.to_owned();
        if value["formatVersion"] != 1 || version.split('.').count() != 4 || version.split('.').any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
            return Err(invalid_data("unsupported runtime manifest"));
        }
        let entries = value["files"].as_object().ok_or_else(|| invalid_data("missing runtime inventory"))?;
        if entries.is_empty() || entries.len() > 10000 { return Err(invalid_data("invalid runtime inventory size")); }
        let mut files = BTreeMap::new();
        for (name, hash) in entries {
            // Portable paths only; disallow alternate streams and separators.
            let path = PathBuf::from(name);
            if name.is_empty() || name.len() > 512 || (name.contains('\\') || name.contains(':')) || path.is_absolute() || path.components().any(|p| !matches!(p, Component::Normal(_))) {
                return Err(invalid_data("invalid runtime inventory path"));
            }
            let hash = hash.as_str().ok_or_else(|| invalid_data("invalid runtime digest"))?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(invalid_data("invalid runtime digest")); }
            files.insert(path, hash.to_ascii_lowercase());
        }
        if !files.contains_key(Path::new("chrome-win64/chrome.exe")) { return Err(invalid_data("runtime inventory omits chrome.exe")); }
        Ok(Self { root, files, version })
    }

    pub(super) fn verify(&self) -> PortResult<()> {
        let mut pending = vec![self.root.clone()];
        let mut observed = BTreeMap::new();
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).map_err(|_| internal("cannot inspect runtime directory"))? {
                let path = entry.map_err(|_| internal("cannot inspect runtime entry"))?.path();
                let metadata = fs::symlink_metadata(&path).map_err(|_| internal("cannot inspect runtime entry"))?;
                if metadata.file_type().is_symlink() || is_reparse_point(&metadata) { return Err(invalid_data("runtime links are not allowed")); }
                if metadata.is_dir() { pending.push(path); continue; }
                let relative = path.strip_prefix(&self.root).map_err(|_| invalid_data("runtime escaped its root"))?;
                if relative == Path::new(MANIFEST) { continue; }
                let expected = self.files.get(relative).ok_or_else(|| invalid_data("unexpected runtime file"))?;
                if !metadata.is_file() || digest_file(&path)? != *expected { return Err(invalid_data("runtime bundle integrity check failed")); }
                observed.insert(relative.to_path_buf(), ());
            }
        }
        if observed.len() != self.files.len() { return Err(invalid_data("runtime bundle is incomplete")); }
        Ok(())
    }
}

/// Only the fixed versioned layout below the core-owned root is enumerated.
/// Interrupted staging installs have a nonnumeric name and are never admitted.
pub fn discover_managed_builds(root: &Path) -> PortResult<Vec<BrowserBuild>> {
    let mut builds = Vec::new();
    for entry in fs::read_dir(root).map_err(|_| internal("cannot inspect managed runtimes"))? {
        let entry = entry.map_err(|_| internal("cannot inspect managed runtime"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.split('.').count() != 4 || name.split('.').any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) { continue; }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| invalid_data("invalid runtime root"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) { return Err(invalid_data("invalid managed runtime root")); }
        let executable = entry.path().join("chrome-win64/chrome.exe");
        let bundle = Bundle::load(&executable)?;
        if bundle.version != name { return Err(invalid_data("runtime version mismatch")); }
        bundle.verify()?;
        let executable = canonical_regular_file(&executable, "managed executable")?;
        if !executable.starts_with(root.canonicalize().map_err(|_| invalid_data("invalid runtime root"))?) { return Err(invalid_data("managed executable escaped root")); }
        builds.push(BrowserBuild {
            id: BrowserBuildId::new(format!("chrome-for-testing-{name}")).map_err(|e| invalid_data(e.to_string()))?,
            provider: "Chrome for Testing".to_owned(), version: name, channel: BrowserChannel::Stable,
            sha256: digest_file(&executable)?, executable_path: executable, publisher: "Google LLC".to_owned(),
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities { supports_language: true, supports_proxy: true, supports_webrtc_policy: true, supports_devtools_pipe: true, fingerprint_support: FingerprintSupport::Unverified },
            installed_at_ms: 0,
        });
    }
    builds.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(builds)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_detects_modified_added_and_removed_files() {
        let root = std::env::temp_dir().join(format!("brows-bundle-{}-{}", std::process::id(), super::super::now_ms()));
        fs::create_dir_all(root.join("chrome-win64")).unwrap();
        let executable = root.join("chrome-win64/chrome.exe");
        fs::write(&executable, b"fixture").unwrap();
        fs::write(root.join(MANIFEST), serde_json::json!({"formatVersion": 1, "version": "150.0.1.2", "files": {"chrome-win64/chrome.exe": digest_file(&executable).unwrap()}}).to_string()).unwrap();
        let bundle = Bundle::load(&executable).unwrap();
        bundle.verify().unwrap();
        fs::write(&executable, b"changed").unwrap();
        assert!(bundle.verify().is_err());
        fs::write(&executable, b"fixture").unwrap();
        fs::write(root.join("extra.dll"), b"extra").unwrap();
        assert!(bundle.verify().is_err());
        fs::remove_file(root.join("extra.dll")).unwrap();
        fs::remove_file(&executable).unwrap();
        assert!(bundle.verify().is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
