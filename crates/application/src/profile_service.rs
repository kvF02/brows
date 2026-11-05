use crate::{
    ApplicationError, ApplicationResult, AuditEvent, AuditOutcome, AuditSink, BrowserRuntime,
    Clock, IdGenerator, PresetCatalog, ProfileQuery, ProfileRepository, ProxyRuntime,
};
use brows_contracts::{
    ErrorCode, ProfileCreateRequest, ProfileDto, ProfileIdRequest, ProfileListRequest,
    ProfileRevisionRequest, ProfileUpdateRequest,
};
use brows_domain::{ProfileId, ProfileSpec, Revision, RuntimeTrust, MAX_PROFILE_NAME_CHARS};
use std::sync::Arc;

#[derive(Debug)]
pub struct ProfileApplicationService {
    profiles: Arc<dyn ProfileRepository>,
    presets: Arc<dyn PresetCatalog>,
    browsers: Arc<dyn BrowserRuntime>,
    proxies: Arc<dyn ProxyRuntime>,
    audit: Arc<dyn AuditSink>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl ProfileApplicationService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profiles: Arc<dyn ProfileRepository>,
        presets: Arc<dyn PresetCatalog>,
        browsers: Arc<dyn BrowserRuntime>,
        proxies: Arc<dyn ProxyRuntime>,
        audit: Arc<dyn AuditSink>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            profiles,
            presets,
            browsers,
            proxies,
            audit,
            clock,
            ids,
        }
    }

    pub fn list(&self, request: ProfileListRequest) -> ApplicationResult<Vec<ProfileDto>> {
        request.validate()?;
        let offset = (request.page as usize - 1).saturating_mul(request.page_size as usize);
        let profiles = self.profiles.list(&ProfileQuery {
            offset,
            limit: request.page_size as usize,
            search: request.search,
            include_archived: request.include_archived,
        })?;
        Ok(profiles.iter().map(ProfileDto::from).collect())
    }

    pub fn get(&self, request: ProfileIdRequest) -> ApplicationResult<ProfileDto> {
        let id: ProfileId = request.profile_id.try_into()?;
        let profile = self.profiles.get(&id)?;
        Ok(ProfileDto::from(&profile))
    }

    pub fn create(&self, request: ProfileCreateRequest) -> ApplicationResult<ProfileDto> {
        let id = self.ids.next_profile_id()?;
        let profile = request.profile.into_domain(id, Revision::FIRST, false)?;
        self.validate_references(&profile)?;
        self.profiles.create(profile.clone())?;
        self.audit_success("profile.create", profile.id.as_str());
        Ok(ProfileDto::from(&profile))
    }

    /// Creates a profile from an authenticated backup manifest. The imported
    /// profile always receives a fresh identity so restoring can never replace
    /// an existing profile's persisted record or userdata directory.
    pub fn create_imported(&self, profile: ProfileSpec) -> ApplicationResult<ProfileDto> {
        self.create_imported_with_data(profile, |_| Ok(()), |_| Ok(()))
    }

    /// Install restored data before publishing a launchable profile record.
    /// Roll back only data installed by this call if metadata publication fails.
    pub fn create_imported_with_data(
        &self,
        mut profile: ProfileSpec,
        install: impl FnOnce(&ProfileId) -> ApplicationResult<()>,
        rollback: impl FnOnce(&ProfileId) -> ApplicationResult<()>,
    ) -> ApplicationResult<ProfileDto> {
        profile.id = self.ids.next_profile_id()?;
        profile.revision = Revision::FIRST;
        profile.archived = false;
        self.validate_references(&profile)?;
        match self.profiles.get(&profile.id) {
            Ok(_) => return Err(ApplicationError::new(ErrorCode::AlreadyExists, "new import identity already exists")),
            Err(error) if error.kind == crate::PortErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
        install(&profile.id)?;
        if let Err(error) = self.profiles.create(profile.clone()) {
            if rollback(&profile.id).is_err() {
                return Err(ApplicationError::new(ErrorCode::Internal, "restored metadata publication and unreferenced data cleanup failed"));
            }
            return Err(error.into());
        }
        self.audit_success("profile.import", profile.id.as_str());
        Ok(ProfileDto::from(&profile))
    }

    pub fn update(&self, request: ProfileUpdateRequest) -> ApplicationResult<ProfileDto> {
        let id: ProfileId = request.profile_id.try_into()?;
        let expected = Revision::new(request.expected_revision)?;
        let current = self.profiles.get(&id)?;
        if current.revision != expected {
            return Err(ApplicationError::new(
                ErrorCode::RevisionConflict,
                format!(
                    "profile revision changed (expected {}, found {})",
                    expected.get(),
                    current.revision.get()
                ),
            ));
        }
        let profile = request
            .profile
            .into_domain(id, expected.next(), current.archived)?;
        self.validate_references(&profile)?;
        self.profiles.update(profile.clone(), expected)?;
        self.audit_success("profile.update", profile.id.as_str());
        Ok(ProfileDto::from(&profile))
    }

    pub fn clone_profile(&self, request: ProfileIdRequest) -> ApplicationResult<ProfileDto> {
        let source_id: ProfileId = request.profile_id.try_into()?;
        let source = self.profiles.get(&source_id)?;
        let mut cloned = source;
        cloned.id = self.ids.next_profile_id()?;
        cloned.name = copy_name(&cloned.name);
        cloned.revision = Revision::FIRST;
        cloned.archived = false;
        cloned.validate()?;
        self.validate_references(&cloned)?;
        self.profiles.create(cloned.clone())?;
        self.audit_success("profile.clone", cloned.id.as_str());
        Ok(ProfileDto::from(&cloned))
    }

    pub fn archive(&self, request: ProfileRevisionRequest) -> ApplicationResult<ProfileDto> {
        self.set_archived(request, true)
    }

    pub fn restore(&self, request: ProfileRevisionRequest) -> ApplicationResult<ProfileDto> {
        self.set_archived(request, false)
    }

    fn set_archived(
        &self,
        request: ProfileRevisionRequest,
        archived: bool,
    ) -> ApplicationResult<ProfileDto> {
        let id: ProfileId = request.profile_id.try_into()?;
        let mut profile = self.profiles.get(&id)?;
        let expected = Revision::new(request.expected_revision)?;
        if profile.revision != expected {
            return Err(ApplicationError::new(
                ErrorCode::RevisionConflict,
                format!(
                    "profile revision changed (expected {}, found {})",
                    expected.get(),
                    profile.revision.get()
                ),
            ));
        }
        if profile.archived == archived {
            return Ok(ProfileDto::from(&profile));
        }
        profile.archived = archived;
        profile.bump_revision();
        self.profiles.update(profile.clone(), expected)?;
        self.audit_success(
            if archived {
                "profile.archive"
            } else {
                "profile.restore"
            },
            profile.id.as_str(),
        );
        Ok(ProfileDto::from(&profile))
    }

    fn validate_references(&self, profile: &ProfileSpec) -> ApplicationResult<()> {
        profile.validate()?;
        let preset = self.presets.resolve(&profile.environment_preset)?;
        preset.validate()?;
        if preset.reference != profile.environment_preset {
            return Err(ApplicationError::new(
                ErrorCode::ValidationFailed,
                "the requested preset version is unavailable",
            ));
        }
        if let Some(proxy_id) = &profile.proxy_id {
            self.proxies.resolve_proxy(proxy_id)?.validate()?;
        }
        let build = self.browsers.resolve_build(&profile.browser)?;
        build.validate()?;
        if build.trust != RuntimeTrust::Verified {
            return Err(ApplicationError::new(
                ErrorCode::RuntimeUntrusted,
                "the selected browser runtime is not verified",
            ));
        }
        if profile.proxy_id.is_some() && !build.capabilities.supports_proxy {
            return Err(ApplicationError::new(
                ErrorCode::CapabilityUnsupported,
                "the selected browser runtime cannot enforce this proxy configuration",
            ));
        }
        Ok(())
    }

    fn audit_success(&self, action: &str, entity_id: &str) {
        // A sink failure is intentionally non-transactional: the primary
        // operation has already committed and must never be falsely reported
        // as failed (which could make a caller retry it).
        let _ = self.audit.record(AuditEvent {
            at_ms: self.clock.now_ms(),
            action: action.to_owned(),
            entity_id: entity_id.to_owned(),
            outcome: AuditOutcome::Succeeded,
        });
    }
}

fn copy_name(original: &str) -> String {
    const SUFFIX: &str = " Copy";
    let max_base_chars = MAX_PROFILE_NAME_CHARS.saturating_sub(SUFFIX.chars().count());
    let mut value: String = original.chars().take(max_base_chars).collect();
    value.push_str(SUFFIX);
    value
}

