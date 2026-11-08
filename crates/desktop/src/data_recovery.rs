//! Startup reconciliation runs while the exclusive data-root lease is held.
use brows_adapters_backup::EncryptedBackupService;
use brows_adapters_morelogin::MoreLoginProfileDataMigrator;
use brows_adapters_profile_data::{DataOperation, OperationKind, ProfileDataJournal};
use brows_adapters_sqlite::SqliteMigrationRecordRepository;
use brows_application::{
    MigrationRecordRepository, MigrationRecordStatus, PortError, PortErrorKind, PortResult,
    ProfileRepository,
};

pub(crate) fn recover(
    journal: &ProfileDataJournal,
    backups: &EncryptedBackupService,
    migrator: &MoreLoginProfileDataMigrator,
    profiles: &dyn ProfileRepository,
    records: &SqliteMigrationRecordRepository,
    now_ms: u64,
) -> PortResult<()> {
    let mut operations = journal.pending()?;
    // Includes pre-intent crashes and interrupted records from older releases.
    for record in records.interrupted()? {
        if operations.iter().any(|op| {
            op.profile_id == record.profile_id.as_str()
                && op.operation_key == format!("migration-{}", record.id)
        }) {
            continue;
        }
        if operations
            .iter()
            .any(|op| op.profile_id == record.profile_id.as_str())
        {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "multiple unfinished operations refer to one profile; data preserved",
            ));
        }
        if record.source_kind != "morelogin" {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "unknown interrupted migration source",
            ));
        }
        let op = DataOperation::new(
            OperationKind::Migration,
            &record.profile_id,
            &format!("migration-{}", record.id),
        )?;
        journal.begin(&op)?;
        operations.push(op);
    }
    for operation in operations {
        let profile = operation.profile()?;
        match operation.kind {
            OperationKind::Restore => {
                let published = match profiles.get(&profile) {
                    Ok(_) => true,
                    Err(error) if error.kind == PortErrorKind::NotFound => false,
                    Err(error) => return Err(error),
                };
                backups.recover_restore(&operation, published)?;
            }
            OperationKind::Migration | OperationKind::Rollback => {
                profiles.get(&profile)?;
                let record = records.get(operation.migration_id()?)?;
                if record.profile_id != profile || record.source_kind != "morelogin" {
                    return Err(PortError::new(
                        PortErrorKind::InvalidData,
                        "recovery record identity mismatch",
                    ));
                }
                match operation.kind {
                    OperationKind::Migration => {
                        let committed = matches!(
                            record.status,
                            MigrationRecordStatus::Completed | MigrationRecordStatus::RolledBack
                        );
                        migrator.recover_operation(&operation, committed)?;
                        if record.status == MigrationRecordStatus::Running {
                            records.fail(
                                record.id,
                                now_ms,
                                "interrupted migration recovered; original data retained",
                            )?;
                        }
                    }
                    OperationKind::Rollback => {
                        if !matches!(
                            record.status,
                            MigrationRecordStatus::Completed | MigrationRecordStatus::RolledBack
                        ) {
                            return Err(PortError::new(
                                PortErrorKind::InvalidData,
                                "rollback recovery status is ambiguous; data preserved",
                            ));
                        }
                        migrator.recover_operation(
                            &operation,
                            record.status == MigrationRecordStatus::RolledBack,
                        )?;
                        if record.status == MigrationRecordStatus::Completed {
                            records.mark_rolled_back(record.id, now_ms)?;
                        }
                    }
                    OperationKind::Restore => unreachable!(),
                }
                journal.finish(&profile)?;
            }
        }
    }
    backups.cleanup_interrupted_staging()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use brows_adapters_profile_data::DataRootLease;
    use brows_adapters_sqlite::SqliteProfileRepository;
    use brows_application::LegacyMigrationArtifact;
    use brows_domain::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
    };

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("brows-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("profiles")).unwrap();
        root.canonicalize().unwrap()
    }
    fn profile() -> ProfileSpec {
        ProfileSpec {
            id: ProfileId::new("recovery-profile").unwrap(),
            name: "Recovery".to_owned(),
            tags: vec![],
            environment_preset: EnvironmentPresetRef::new(
                PresetId::new("system-native").unwrap(),
                1,
            )
            .unwrap(),
            proxy_id: None,
            browser: BrowserBuildSelector::Channel(BrowserChannel::Stable),
            persona: FingerprintPersona::SystemNative(SystemNativePersona::default()),
            locale: LocaleConfig {
                accept_languages: vec!["en-US".to_owned()],
                timezone: None,
            },
            window: WindowConfig {
                width: 1280,
                height: 800,
                start_maximized: false,
            },
            startup_urls: vec![],
            revision: Revision::FIRST,
            archived: false,
        }
    }
    fn generation(path: &Path, content: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("identity"), content).unwrap();
    }

    #[test]
    fn restore_recovery_obeys_real_sqlite_publication_state() {
        for published in [false, true] {
            let root = root();
            {
                let journal = Arc::new(
                    ProfileDataJournal::new(
                        root.join("profiles"),
                        Some(DataRootLease::acquire(&root).unwrap()),
                    )
                    .unwrap(),
                );
                let backups =
                    EncryptedBackupService::new(root.join("backups"), root.join("profiles"))
                        .unwrap()
                        .with_journal(journal.clone())
                        .unwrap();
                let migrator =
                    MoreLoginProfileDataMigrator::new(root.join("source"), root.join("profiles"))
                        .unwrap()
                        .with_journal(journal.clone())
                        .unwrap();
                let profiles = SqliteProfileRepository::open(root.join("db.sqlite3")).unwrap();
                let records =
                    SqliteMigrationRecordRepository::open(root.join("db.sqlite3")).unwrap();
                let profile = profile();
                let stage =
                    root.join("backups/restore-staging/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/userdata");
                generation(&stage, "restored");
                backups
                    .install_restored_data(profile.id.as_str(), &stage)
                    .unwrap();
                if published {
                    profiles.create(profile.clone()).unwrap();
                }
                recover(&journal, &backups, &migrator, &profiles, &records, 100).unwrap();
                recover(&journal, &backups, &migrator, &profiles, &records, 100).unwrap();
                let target = journal.profile_dir(&profile.id).join("userdata");
                assert_eq!(target.exists(), published);
                assert!(journal.pending().unwrap().is_empty());
                if published {
                    assert_eq!(
                        fs::read_to_string(target.join("identity")).unwrap(),
                        "restored"
                    );
                }
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn migration_recovery_reconciles_files_and_committed_records() {
        for committed in [false, true] {
            let root = root();
            {
                let journal = Arc::new(
                    ProfileDataJournal::new(
                        root.join("profiles"),
                        Some(DataRootLease::acquire(&root).unwrap()),
                    )
                    .unwrap(),
                );
                let backups =
                    EncryptedBackupService::new(root.join("backups"), root.join("profiles"))
                        .unwrap()
                        .with_journal(journal.clone())
                        .unwrap();
                let migrator =
                    MoreLoginProfileDataMigrator::new(root.join("source"), root.join("profiles"))
                        .unwrap()
                        .with_journal(journal.clone())
                        .unwrap();
                let profiles = SqliteProfileRepository::open(root.join("db.sqlite3")).unwrap();
                let records =
                    SqliteMigrationRecordRepository::open(root.join("db.sqlite3")).unwrap();
                let profile = profile();
                profiles.create(profile.clone()).unwrap();
                let record = records
                    .begin(&profile.id, "morelogin", "12345678", 1)
                    .unwrap();
                let key = format!("migration-{}", record.id);
                let dir = journal.profile_dir(&profile.id);
                fs::create_dir_all(&dir).unwrap();
                generation(&dir.join("userdata"), "new");
                generation(&dir.join(format!("userdata.backup-{key}")), "old");
                if committed {
                    journal
                        .begin(
                            &DataOperation::new(OperationKind::Migration, &profile.id, &key)
                                .unwrap(),
                        )
                        .unwrap();
                    records
                        .complete(
                            record.id,
                            2,
                            &LegacyMigrationArtifact {
                                backup_key: format!("userdata.backup-{key}"),
                                copied_files: 1,
                                copied_bytes: 3,
                            },
                        )
                        .unwrap();
                }
                // The noncommitted branch also covers older/pre-intent running rows.
                recover(&journal, &backups, &migrator, &profiles, &records, 100).unwrap();
                recover(&journal, &backups, &migrator, &profiles, &records, 100).unwrap();
                assert_eq!(
                    fs::read_to_string(dir.join("userdata/identity")).unwrap(),
                    if committed { "new" } else { "old" }
                );
                assert_eq!(
                    records.get(record.id).unwrap().status,
                    if committed {
                        MigrationRecordStatus::Completed
                    } else {
                        MigrationRecordStatus::Failed
                    }
                );
                if !committed {
                    assert_eq!(
                        fs::read_to_string(
                            dir.join(format!("userdata.after-rollback-{key}/identity"))
                        )
                        .unwrap(),
                        "new"
                    );
                }
                assert!(journal.pending().unwrap().is_empty());
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn rollback_recovery_finishes_metadata_without_repeating_directory_swaps() {
        let root = root();
        {
            let journal = Arc::new(
                ProfileDataJournal::new(
                    root.join("profiles"),
                    Some(DataRootLease::acquire(&root).unwrap()),
                )
                .unwrap(),
            );
            let backups = EncryptedBackupService::new(root.join("backups"), root.join("profiles"))
                .unwrap()
                .with_journal(journal.clone())
                .unwrap();
            let migrator =
                MoreLoginProfileDataMigrator::new(root.join("source"), root.join("profiles"))
                    .unwrap()
                    .with_journal(journal.clone())
                    .unwrap();
            let profiles = SqliteProfileRepository::open(root.join("db.sqlite3")).unwrap();
            let records = SqliteMigrationRecordRepository::open(root.join("db.sqlite3")).unwrap();
            let profile = profile();
            profiles.create(profile.clone()).unwrap();
            let record = records
                .begin(&profile.id, "morelogin", "12345678", 1)
                .unwrap();
            let key = format!("migration-{}", record.id);
            records
                .complete(
                    record.id,
                    2,
                    &LegacyMigrationArtifact {
                        backup_key: format!("userdata.backup-{key}"),
                        copied_files: 1,
                        copied_bytes: 3,
                    },
                )
                .unwrap();
            journal
                .begin(&DataOperation::new(OperationKind::Rollback, &profile.id, &key).unwrap())
                .unwrap();
            let dir = journal.profile_dir(&profile.id);
            generation(&dir.join("userdata"), "old");
            generation(&dir.join(format!("userdata.after-rollback-{key}")), "new");
            recover(&journal, &backups, &migrator, &profiles, &records, 100).unwrap();
            recover(&journal, &backups, &migrator, &profiles, &records, 100).unwrap();
            assert_eq!(
                records.get(record.id).unwrap().status,
                MigrationRecordStatus::RolledBack
            );
            assert_eq!(
                fs::read_to_string(dir.join("userdata/identity")).unwrap(),
                "old"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
