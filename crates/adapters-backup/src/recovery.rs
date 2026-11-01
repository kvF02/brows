use super::*;

impl EncryptedBackupService {
    pub(super) fn staging_root(&self, create: bool) -> PortResult<Option<PathBuf>> {
        let root = self.backup_root.join("restore-staging");
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() && !is_reparse_or_symlink(&metadata) => {}
            Ok(_) => return Err(invalid("restore staging root is not a regular directory")),
            Err(error) if error.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&root).map_err(|e| io_error("create restore staging root", e))?;
            }
            Err(error) => return Err(io_error("inspect restore staging root", error)),
        }
        let canonical = root
            .canonicalize()
            .map_err(|e| io_error("resolve restore staging root", e))?;
        ensure_within(&canonical, &self.backup_root, "restore staging root")?;
        Ok(Some(canonical))
    }

    pub(super) fn task_staging(&self, id: u64) -> PortResult<PathBuf> {
        if id == 0 {
            return Err(invalid("invalid restore task id"));
        }
        let root = self
            .staging_root(true)?
            .ok_or_else(|| invalid("staging root missing"))?;
        let staging = root.join(format!("task-{id:016x}"));
        fs::create_dir(&staging).map_err(|e| io_error("allocate restore task staging", e))?;
        Ok(staging)
    }

    pub(super) fn validate_staging_data(&self, data: &Path) -> PortResult<PathBuf> {
        let root = self
            .staging_root(false)?
            .ok_or_else(|| invalid("staging root missing"))?;
        let parent = data
            .parent()
            .ok_or_else(|| invalid("staging data has no parent"))?;
        let key = parent
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid("invalid staging key"))?;
        if data.file_name() != Some(OsStr::new("userdata"))
            || parent.parent() != Some(root.as_path())
            || !managed_key(key)
        {
            return Err(invalid("restored data is outside managed staging"));
        }
        for path in [parent, data] {
            let metadata = fs::symlink_metadata(path)
                .map_err(|e| io_error("inspect restored directory", e))?;
            if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
                return Err(invalid("restored directory is a link or non-directory"));
            }
        }
        let canonical = data
            .canonicalize()
            .map_err(|e| io_error("resolve restored directory", e))?;
        inspect_source_tree(&canonical, &root)?;
        Ok(canonical)
    }

    pub(super) fn remove_staging_named(&self, key: &str) -> PortResult<()> {
        if !managed_key(key) {
            return Err(invalid("unrecognized restore staging entry"));
        }
        let Some(root) = self.staging_root(false)? else {
            return Ok(());
        };
        let directory = root.join(key);
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error("inspect restore staging entry", error)),
        };
        if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
            return Err(invalid("restore staging entry is not a regular directory"));
        }
        // Only app-generated layouts are eligible; preserve unknown user files.
        for entry in
            fs::read_dir(&directory).map_err(|e| io_error("inspect restore staging content", e))?
        {
            if entry
                .map_err(|e| io_error("inspect restore staging content", e))?
                .file_name()
                != OsStr::new("userdata")
            {
                return Err(invalid("restore staging contains unexpected content"));
            }
        }
        inspect_source_tree(&directory, &root)?;
        fs::remove_dir_all(&directory).map_err(|e| io_error("remove restore staging", e))
    }

    pub(super) fn cleanup_task_staging(&self, id: u64) -> PortResult<()> {
        if id == 0 {
            return Err(invalid("invalid restore task id"));
        }
        self.remove_staging_named(&format!("task-{id:016x}"))
    }

    pub(super) fn sweep_staging(&self) -> PortResult<bool> {
        let Some(root) = self.staging_root(false)? else {
            return Ok(false);
        };
        let mut blocked = false;
        for entry in fs::read_dir(root).map_err(|e| io_error("list restore staging", e))? {
            let entry = entry.map_err(|e| io_error("read restore staging entry", e))?;
            let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
                blocked = true;
                continue;
            };
            if !managed_key(&key) || self.remove_staging_named(&key).is_err() {
                blocked = true;
            }
        }
        Ok(blocked)
    }
}

fn managed_key(key: &str) -> bool {
    let hex = |value: &str| {
        value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    (key.len() == 32 && hex(key))
        || key
            .strip_prefix("task-")
            .is_some_and(|id| id.len() == 16 && hex(id) && id != "0000000000000000")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_removes_only_managed_staging_and_preserves_archives_and_profile_data() {
        let (service, root) = crate::tests::service("cleanup-boundaries");
        let staging = service.staging_root(true).unwrap().unwrap();
        let task = service.task_staging(1).unwrap();
        fs::create_dir_all(task.join("userdata/Default")).unwrap();
        fs::write(task.join("userdata/Default/partial"), b"partial").unwrap();
        let legacy = staging.join("a".repeat(32));
        fs::create_dir_all(legacy.join("userdata")).unwrap();
        let unknown = staging.join("user-notes");
        fs::create_dir_all(&unknown).unwrap();
        fs::write(unknown.join("notes"), b"keep").unwrap();
        let archive = service.backup_root.join("existing.browsbak");
        fs::write(&archive, b"keep archive").unwrap();
        let data = service.profile_root.join("existing-profile");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("cookie"), b"keep profile").unwrap();
        assert!(service.sweep_staging().unwrap());
        assert!(!task.exists());
        assert!(!legacy.exists());
        assert_eq!(fs::read(unknown.join("notes")).unwrap(), b"keep");
        assert_eq!(fs::read(archive).unwrap(), b"keep archive");
        assert_eq!(fs::read(data.join("cookie")).unwrap(), b"keep profile");
        service.cleanup_task_staging(1).unwrap();
        assert!(service.cleanup_task_staging(0).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unexpected_content_is_preserved_and_cleanup_can_be_retried() {
        let (service, root) = crate::tests::service("cleanup-retry");
        let task = service.task_staging(2).unwrap();
        fs::write(task.join("unknown-file"), b"do not delete").unwrap();
        assert!(service.cleanup_task_staging(2).is_err());
        assert_eq!(
            fs::read(task.join("unknown-file")).unwrap(),
            b"do not delete"
        );
        fs::remove_file(task.join("unknown-file")).unwrap();
        service.cleanup_task_staging(2).unwrap();
        assert!(!service.sweep_staging().unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn install_never_replaces_existing_data_and_discard_rejects_foreign_paths() {
        let (service, root) = crate::tests::service("install-boundaries");
        let task = service.task_staging(3).unwrap();
        fs::create_dir_all(task.join("userdata")).unwrap();
        fs::write(task.join("userdata/new"), b"new").unwrap();
        let target = service.profile_data_path("existing");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("old"), b"old").unwrap();
        assert!(service
            .install_restored_data("existing", &task.join("userdata"))
            .is_err());
        assert_eq!(fs::read(target.join("old")).unwrap(), b"old");
        assert!(task.join("userdata/new").exists());
        service.discard_restored_data(&target);
        assert_eq!(fs::read(target.join("old")).unwrap(), b"old");
        service.discard_restored_data(&task.join("not-userdata"));
        assert!(task.exists());
        service.cleanup_task_staging(3).unwrap();
        assert_eq!(fs::read(target.join("old")).unwrap(), b"old");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn directory_link(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }
    #[cfg(windows)]
    fn directory_link(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_dir(target, link).expect(
            "directory-link regression requires Developer Mode or a privileged test runner",
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn cleanup_rejects_linked_roots_entries_and_nested_links() {
        for location in 0..3 {
            let (service, root) = crate::tests::service(&format!("link-{location}"));
            let outside = root.join("outside");
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("sentinel"), b"keep").unwrap();
            match location {
                0 => {
                    directory_link(&outside, &service.backup_root.join("restore-staging"));
                }
                1 => {
                    let staging = service.staging_root(true).unwrap().unwrap();
                    directory_link(&outside, &staging.join("task-0000000000000001"));
                }
                _ => {
                    let task = service.task_staging(1).unwrap();
                    fs::create_dir_all(task.join("userdata")).unwrap();
                    directory_link(&outside, &task.join("userdata/link"));
                }
            }
            assert!(service.cleanup_task_staging(1).is_err());
            assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
            let _ = fs::remove_dir_all(root);
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn install_rejects_a_linked_profile_parent() {
        let (service, root) = crate::tests::service("install-link");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let parent = service
            .profile_data_path("fresh")
            .parent()
            .unwrap()
            .to_owned();
        directory_link(&outside, &parent);
        let task = service.task_staging(1).unwrap();
        fs::create_dir_all(task.join("userdata")).unwrap();
        assert!(service
            .install_restored_data("fresh", &task.join("userdata"))
            .is_err());
        assert!(!outside.join("userdata").exists());
        let _ = fs::remove_dir_all(root);
    }
}
