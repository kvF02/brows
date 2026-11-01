use super::*;

impl EncryptedBackupService {
    fn export_root(&self, create: bool) -> PortResult<Option<PathBuf>> {
        let path = self.backup_root.join("export-staging");
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !is_reparse_or_symlink(&metadata) => {}
            Ok(_) => return Err(invalid("export staging root is linked or invalid")),
            Err(error) if error.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(|e| io_error("create export staging root", e))?;
            }
            Err(error) => return Err(io_error("inspect export staging root", error)),
        }
        let canonical = path
            .canonicalize()
            .map_err(|e| io_error("resolve export staging root", e))?;
        ensure_within(&canonical, &self.backup_root, "export staging root")?;
        Ok(Some(canonical))
    }

    pub(super) fn allocate_export(&self) -> PortResult<PathBuf> {
        let root = self
            .export_root(true)?
            .ok_or_else(|| invalid("export staging root missing"))?;
        unique_staging_directory(&root)
    }

    pub(super) fn remove_export(&self, directory: &Path) -> PortResult<()> {
        let root = self
            .export_root(false)?
            .ok_or_else(|| invalid("export staging root missing"))?;
        let key = directory
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        if directory.parent() != Some(root.as_path())
            || key.len() != 32
            || !key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("unrecognized export staging directory"));
        }
        let metadata =
            fs::symlink_metadata(directory).map_err(|e| io_error("inspect export directory", e))?;
        if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
            return Err(invalid("linked export directory"));
        }
        // An interrupted export owns only one ciphertext file. Never recurse,
        // follow a link, or remove unknown content even in a recognized folder.
        for entry in fs::read_dir(directory).map_err(|e| io_error("inspect export content", e))? {
            let entry = entry.map_err(|e| io_error("inspect export entry", e))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|e| io_error("inspect export file", e))?;
            if entry.file_name() != OsStr::new("archive.partial")
                || !metadata.is_file()
                || is_reparse_or_symlink(&metadata)
            {
                return Err(invalid("export staging contains unexpected content"));
            }
        }
        let partial = directory.join("archive.partial");
        match fs::remove_file(partial) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("remove incomplete export", error)),
        }
        fs::remove_dir(directory).map_err(|e| io_error("remove export staging directory", e))
    }

    pub(super) fn sweep_exports(&self) -> PortResult<bool> {
        let Some(root) = self.export_root(false)? else {
            self.export_cleanup_warning
                .store(false, std::sync::atomic::Ordering::Release);
            return Ok(false);
        };
        let mut pending = false;
        for entry in fs::read_dir(root).map_err(|e| io_error("list incomplete exports", e))? {
            let entry = entry.map_err(|e| io_error("read incomplete export", e))?;
            if self.remove_export(&entry.path()).is_err() {
                pending = true;
            }
        }
        self.export_cleanup_warning
            .store(pending, std::sync::atomic::Ordering::Release);
        Ok(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_removes_only_owned_ciphertext_and_preserves_completed_and_legacy_files() {
        let (service, root) = crate::tests::service("export-recovery");
        let incomplete = service.allocate_export().unwrap();
        fs::write(
            incomplete.join("archive.partial"),
            b"interrupted ciphertext",
        )
        .unwrap();
        let published = service.allocate_export().unwrap(); // Rename succeeded before a crash.
        fs::write(root.join("backups/finished.browsbak"), b"completed").unwrap();
        fs::write(root.join("backups/legacy.browsbak.partial"), b"unowned").unwrap();
        assert!(!service.sweep_exports().unwrap());
        assert!(!incomplete.exists());
        assert!(!published.exists());
        assert!(!service.sweep_exports().unwrap());
        assert_eq!(
            fs::read(root.join("backups/finished.browsbak")).unwrap(),
            b"completed"
        );
        assert_eq!(
            fs::read(root.join("backups/legacy.browsbak.partial")).unwrap(),
            b"unowned"
        );
        let unknown = service.allocate_export().unwrap();
        fs::write(unknown.join("notes"), b"preserve").unwrap();
        assert!(service.sweep_exports().unwrap());
        assert_eq!(fs::read(unknown.join("notes")).unwrap(), b"preserve");
        fs::remove_file(unknown.join("notes")).unwrap();
        assert!(!service.sweep_exports().unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_rejects_export_links_and_does_not_touch_their_targets() {
        let (service, root) = crate::tests::service("export-links");
        let directory = service.allocate_export().unwrap();
        let outside = root.join("keep");
        fs::write(&outside, b"keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, directory.join("archive.partial")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, directory.join("archive.partial")).unwrap();
        assert!(service.sweep_exports().unwrap());
        assert_eq!(fs::read(outside).unwrap(), b"keep");
        fs::remove_file(directory.join("archive.partial")).unwrap();
        assert!(!service.sweep_exports().unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
