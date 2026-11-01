use super::*;

impl MoreLoginProfileDataMigrator {
    pub(super) fn recover_migration(&self, profile: &ProfileId, operation: &str) -> PortResult<()> {
        validate_operation_key(operation)?;
        let directory = self.profile_dir(profile)?;
        let target = directory.join("userdata");
        let staging = directory.join(format!("userdata.staging-{operation}"));
        let backup = directory.join(format!("userdata.backup-{operation}"));
        let preserved = directory.join(format!("userdata.interrupted-{operation}"));
        let target_exists = checked_tree(&target)?;
        let staging_exists = checked_tree(&staging)?;
        let backup_exists = checked_tree(&backup)?;
        let preserved_exists = checked_tree(&preserved)?;
        // Validate every candidate before touching any data. A second recovery
        // can resume after either rename, without re-reading the MoreLogin source.
        if backup_exists {
            if staging_exists && preserved_exists
                || target_exists && (staging_exists || preserved_exists)
            {
                return Err(ambiguous());
            }
            if target_exists {
                fs::rename(&target, &preserved)
                    .map_err(|e| io_error("preserve interrupted migration", e))?;
            }
            fs::rename(&backup, &target)
                .map_err(|e| io_error("restore interrupted migration backup", e))?;
        } else if !target_exists && (staging_exists || preserved_exists) {
            return Err(ambiguous());
        }
        if staging_exists {
            fs::remove_dir_all(staging)
                .map_err(|e| io_error("remove incomplete migration copy", e))?;
        }
        Ok(())
    }

    pub(super) fn recover_rollback(&self, profile: &ProfileId, key: &str) -> PortResult<()> {
        validate_backup_key(key)?;
        let directory = self.profile_dir(profile)?;
        let target = directory.join("userdata");
        let backup = directory.join(key);
        let after = directory.join(format!("userdata.after-rollback-{}", &key[16..]));
        let target_exists = checked_tree(&target)?;
        let backup_exists = checked_tree(&backup)?;
        let after_exists = checked_tree(&after)?;
        match (target_exists, backup_exists, after_exists) {
            (true, true, false) => {
                fs::rename(&target, &after)
                    .map_err(|e| io_error("preserve migrated profile", e))?;
                fs::rename(&backup, &target)
                    .map_err(|e| io_error("restore migration backup", e))?;
            }
            (false, true, true) => {
                fs::rename(&backup, &target)
                    .map_err(|e| io_error("finish interrupted rollback", e))?;
            }
            (true, false, true) => {} // Installed before the journal acknowledgement.
            _ => return Err(ambiguous()),
        }
        Ok(())
    }
}

fn ambiguous() -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        "migration recovery requires inspection; all data was preserved",
    )
}

pub(super) fn checked_tree(root: &Path) -> PortResult<bool> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error("inspect migration recovery root", error)),
        Ok(metadata) if is_link(&metadata) || !metadata.is_dir() => return Err(ambiguous()),
        Ok(_) => {}
    }
    let mut paths = vec![root.to_path_buf()];
    let mut count = 0u64;
    while let Some(path) = paths.pop() {
        for entry in fs::read_dir(path).map_err(|e| io_error("inspect recovery content", e))? {
            let entry = entry.map_err(|e| io_error("inspect recovery entry", e))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|e| io_error("inspect recovery type", e))?;
            count += 1;
            if count > MAX_MIGRATION_FILES || is_link(&metadata) {
                return Err(ambiguous());
            }
            if metadata.is_dir() {
                paths.push(entry.path());
            } else if !metadata.is_file() {
                return Err(ambiguous());
            }
        }
    }
    Ok(true)
}

pub(super) fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> (MoreLoginProfileDataMigrator, ProfileId, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "brows-migration-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let migrator =
            MoreLoginProfileDataMigrator::new(root.join("missing-source"), root.join("profiles"))
                .unwrap();
        let profile = ProfileId::new("recovery-profile").unwrap();
        let directory = migrator.profile_dir(&profile).unwrap();
        (migrator, profile, directory, root)
    }
    fn state(path: &Path, content: &[u8]) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("state"), content).unwrap();
    }

    #[test]
    fn every_forward_rename_boundary_recovers_original_and_preserves_installed_data() {
        for phase in 0..5 {
            let (migrator, profile, directory, root) = fixture("forward");
            let target = directory.join("userdata");
            let staging = directory.join("userdata.staging-migration-7");
            let backup = directory.join("userdata.backup-migration-7");
            let interrupted = directory.join("userdata.interrupted-migration-7");
            match phase {
                0 => state(&target, b"original"),
                1 => {
                    state(&target, b"original");
                    state(&staging, b"partial");
                }
                2 => {
                    state(&backup, b"original");
                    state(&staging, b"copy");
                }
                3 => {
                    state(&backup, b"original");
                    state(&target, b"installed");
                }
                4 => {
                    state(&backup, b"original");
                    state(&interrupted, b"installed");
                }
                _ => unreachable!(),
            }
            state(&root.join("unrelated"), b"keep");
            migrator.recover(&profile, "migration-7").unwrap();
            migrator.recover(&profile, "migration-7").unwrap();
            assert_eq!(fs::read(target.join("state")).unwrap(), b"original");
            assert!(!staging.exists());
            assert!(!backup.exists());
            if phase >= 3 {
                assert_eq!(fs::read(interrupted.join("state")).unwrap(), b"installed");
            }
            assert_eq!(fs::read(root.join("unrelated/state")).unwrap(), b"keep");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn rollback_can_resume_after_either_rename_without_deleting_either_version() {
        for phase in 0..3 {
            let (migrator, profile, directory, root) = fixture("rollback");
            let target = directory.join("userdata");
            let backup = directory.join("userdata.backup-migration-9");
            let after = directory.join("userdata.after-rollback-migration-9");
            match phase {
                0 => {
                    state(&target, b"migrated");
                    state(&backup, b"original");
                }
                1 => {
                    state(&after, b"migrated");
                    state(&backup, b"original");
                }
                2 => {
                    state(&after, b"migrated");
                    state(&target, b"original");
                }
                _ => unreachable!(),
            }
            migrator
                .rollback(&profile, "userdata.backup-migration-9")
                .unwrap();
            migrator
                .rollback(&profile, "userdata.backup-migration-9")
                .unwrap();
            assert_eq!(fs::read(target.join("state")).unwrap(), b"original");
            assert_eq!(fs::read(after.join("state")).unwrap(), b"migrated");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn ambiguous_layout_and_linked_data_are_preserved_without_cross_profile_changes() {
        let (migrator, profile, directory, root) = fixture("ambiguous");
        let target = directory.join("userdata");
        let backup = directory.join("userdata.backup-migration-1");
        let staging = directory.join("userdata.staging-migration-1");
        state(&target, b"target");
        state(&backup, b"backup");
        state(&staging, b"staging");
        assert!(migrator.recover(&profile, "migration-1").is_err());
        assert_eq!(fs::read(target.join("state")).unwrap(), b"target");
        assert_eq!(fs::read(backup.join("state")).unwrap(), b"backup");
        fs::remove_dir_all(&staging).unwrap();
        let other = root.join("other");
        state(&other, b"other profile");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, target.join("link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&other, target.join("link")).unwrap();
        assert!(migrator.recover(&profile, "migration-1").is_err());
        assert_eq!(fs::read(other.join("state")).unwrap(), b"other profile");
        #[cfg(unix)]
        fs::remove_file(target.join("link")).unwrap();
        #[cfg(windows)]
        fs::remove_dir(target.join("link")).unwrap();
        migrator.recover(&profile, "migration-1").unwrap();
        assert_eq!(fs::read(target.join("state")).unwrap(), b"backup");
        fs::remove_dir_all(root).unwrap();
    }
}
