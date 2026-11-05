use super::{invalid_data, is_reparse_point};
use brows_application::{PortError, PortErrorKind, PortResult};
use brows_domain::BrowserBuild;
use std::{fs, path::Path};

const PREFIX: &str = ".brows-runtime-floor-";

pub(crate) fn version_key(value: &str) -> PortResult<[u16; 4]> {
    let values: Vec<_> = value.split('.').map(str::parse::<u16>).collect::<Result<_, _>>()
        .map_err(|_| invalid_data("runtime version must contain four native version components"))?;
    values.try_into().map_err(|_| invalid_data("runtime version must contain four native version components"))
}

/// Append-only local admission stamps prevent a tracked profile being reopened
/// with an older build or a different browser family. Legacy unstamped profiles
/// establish their first floor here; this cannot reconstruct their earlier use.
pub(crate) fn admit(directory: &Path, build: &BrowserBuild) -> PortResult<()> {
    let family = match build.provider.as_str() {
        "Google Chrome" => "chrome",
        "Microsoft Edge" => "edge",
        // Other adapters must define their own compatibility policy.
        _ => return Ok(()),
    };
    let version = version_key(&build.version)?;
    for (index, entry) in fs::read_dir(directory).map_err(|_| invalid_data("cannot read runtime history"))?.enumerate() {
        if index > 100_000 { return Err(invalid_data("profile directory exceeds admission budget")); }
        let entry = entry.map_err(|_| invalid_data("cannot inspect runtime history"))?;
        let name = entry.file_name();
        let Some(stamp) = name.to_str().and_then(|name| name.strip_prefix(PREFIX)) else { continue };
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| invalid_data("cannot inspect runtime stamp"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) || metadata.len() != 0 {
            return Err(invalid_data("invalid runtime history stamp"));
        }
        let (previous_family, previous_version) = stamp.split_once('-').ok_or_else(|| invalid_data("invalid runtime history stamp"))?;
        let previous = version_key(previous_version)?;
        if previous_family != family || previous > version {
            return Err(PortError::new(PortErrorKind::RuntimeUntrusted,
                "profile was admitted to another browser family or newer version; use a fresh profile or restore a matching backup"));
        }
    }
    let stamp = directory.join(format!("{PREFIX}{family}-{}", build.version));
    match fs::OpenOptions::new().write(true).create_new(true).open(stamp) {
        Ok(file) => file.sync_all().map_err(|_| invalid_data("cannot persist runtime history")),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(_) => Err(invalid_data("cannot create runtime history")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_history_blocks_downgrades_and_cross_family_reuse() {
        let root = std::env::temp_dir().join(format!("brows-floor-{}-{}", std::process::id(), super::super::now_ms()));
        fs::create_dir(&root).unwrap();
        let mut build = super::super::tests::sample_build("test", root.join("chrome.exe"), b"unused", brows_domain::BrowserChannel::Stable);
        build.provider = "Google Chrome".to_owned();
        build.version = "150.0.0.2".to_owned();
        admit(&root, &build).unwrap();
        admit(&root, &build).unwrap();
        build.version = "150.0.0.1".to_owned();
        assert!(admit(&root, &build).is_err());
        build.version = "151.0.0.0".to_owned();
        admit(&root, &build).unwrap();
        build.provider = "Microsoft Edge".to_owned();
        assert!(admit(&root, &build).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
