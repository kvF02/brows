use crate::{
    ApplicationResult, BackupExportSpec, BackupImportSpec, ProfileApplicationService,
    RestorableBackupService, SecretValue, SessionApplicationService,
};
use brows_contracts::{ProfileDto, ProfileIdRequest};
use brows_domain::ProfileId;
use std::sync::Arc;

/// Coordinates profile ownership with archive I/O. Transport handlers only map DTOs.
#[derive(Debug)]
pub struct BackupApplicationService {
    profiles: Arc<ProfileApplicationService>,
    sessions: Arc<SessionApplicationService>,
    archives: Arc<dyn RestorableBackupService>,
}

impl BackupApplicationService {
    pub fn new(
        profiles: Arc<ProfileApplicationService>,
        sessions: Arc<SessionApplicationService>,
        archives: Arc<dyn RestorableBackupService>,
    ) -> Self {
        Self {
            profiles,
            sessions,
            archives,
        }
    }

    pub fn export(
        &self,
        id: ProfileId,
        destination_name: String,
        password: SecretValue,
        include_proxy_credential: bool,
    ) -> ApplicationResult<ProfileDto> {
        let _reservation = self.sessions.reserve_stopped_profile(&id)?;
        let profile = self.profiles.get(ProfileIdRequest {
            profile_id: (&id).into(),
        })?;
        self.archives.export(&BackupExportSpec {
            profile: profile.clone().try_into()?,
            destination_name,
            password,
            include_proxy_credential,
        })?;
        Ok(profile)
    }

    pub fn import(&self, spec: BackupImportSpec) -> ApplicationResult<ProfileDto> {
        let payload = self.archives.import(&spec)?;
        // Cleanup also runs when profile validation or persistence fails.
        let _staging = RestoreStaging {
            archives: self.archives.as_ref(),
            path: &payload.restored_data_dir,
        };
        self.profiles.create_imported_with(payload.profile, |id| {
            Ok(self
                .archives
                .install_restored_data(id, &payload.restored_data_dir)?)
        })
    }
}

struct RestoreStaging<'a> {
    archives: &'a dyn RestorableBackupService,
    path: &'a std::path::Path,
}

impl Drop for RestoreStaging<'_> {
    fn drop(&mut self) {
        self.archives.discard_restored_data(self.path);
    }
}
