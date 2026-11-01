//! Preserve validated filesystem identity at the Chromium command-line boundary.
use super::*;

fn compatible(path: &Path) -> PathBuf {
    let short = dunce::simplified(path);
    if short != path {
        return short.to_path_buf();
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;
        // dunce deliberately retains verbatim paths beyond MAX_PATH. Chromium
        // needs the ordinary absolute spelling there too. Strip only a drive
        // prefix, and only after proving the OS resolves it to the same object.
        // UNC/device paths and names with different legacy meanings stay intact.
        if matches!(path.components().next(),Some(Component::Prefix(prefix)) if matches!(prefix.kind(),Prefix::VerbatimDisk(_)))
        {
            if let Some(text) = path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
                let ordinary = PathBuf::from(text);
                if let (Ok(left), Ok(right)) = (ordinary.canonicalize(), path.canonicalize()) {
                    if left == right {
                        return ordinary;
                    }
                }
            }
        }
    }
    path.to_path_buf()
}

pub(super) fn argument(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(compatible(path));
    argument
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_arguments_are_single_lossless_os_strings() {
        let path = PathBuf::from("directory with spaces/one profile");
        let mut expected = OsString::from("--user-data-dir=");
        expected.push(&path);
        assert_eq!(argument("--user-data-dir=", &path), expected);
    }
    #[cfg(windows)]
    #[test]
    fn long_verbatim_drive_paths_keep_filesystem_identity_in_chromium_arguments() {
        let root = std::env::temp_dir().join(format!(
            "brows-command-path-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let long = (0..4).fold(root.clone(), |p, i| {
            p.join(format!("segment-{i}-{}", "x".repeat(65)))
        });
        fs::create_dir_all(&long).unwrap();
        let canonical = long.canonicalize().unwrap();
        assert!(canonical.as_os_str().len() > 260);
        let ordinary = compatible(&canonical);
        assert!(!ordinary.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(ordinary.canonicalize().unwrap(), canonical);
        let file = ordinary.join("manifest.json");
        fs::write(&file, b"{}").unwrap();
        assert_eq!(fs::read(canonical.join("manifest.json")).unwrap(), b"{}");
        fs::remove_dir_all(root).unwrap();
    }
}
