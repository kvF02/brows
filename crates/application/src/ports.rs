use brows_domain::{
    BrowserBuild, BrowserBuildSelector, DiagnosticReport, EnvironmentPreset, EnvironmentPresetRef,
    FingerprintPersona, LaunchArgument, LaunchWarning, ProfileId, ProfileSpec, ProxyId,
    ProxyLaunchBinding, ProxySpec, ResolvedLaunchPlan, Revision, SecretRef, SessionId,
};
use std::{fmt, path::PathBuf};

pub type PortResult<T> = Result<T, PortError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortErrorKind {
    NotFound,
    AlreadyExists,
    RevisionConflict,
    Locked,
    CapabilityUnsupported,
    ProxyConnect,
    ProxyAuth,
    RuntimeUntrusted,
    RuntimeInUse,
    WrongPassword,
    Tampered,
    Incompatible,
    InvalidData,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortError {
    pub kind: PortErrorKind,
    pub message: String,
}

impl PortError {
    pub fn new(kind: PortErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn not_found(entity: &'static str) -> Self {
        Self::new(PortErrorKind::NotFound, format!("{entity} was not found"))
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PortError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProfileQuery {
    pub offset: usize,
    pub limit: usize,
    pub search: Option<String>,
    pub include_archived: bool,
}

pub trait ProfileRepository: fmt::Debug + Send + Sync {
    fn list(&self, query: &ProfileQuery) -> PortResult<Vec<ProfileSpec>>;
    fn get(&self, id: &ProfileId) -> PortResult<ProfileSpec>;
    fn create(&self, profile: ProfileSpec) -> PortResult<()>;
    /// Atomically replaces the profile if its persisted revision equals
    /// `expected_revision`.
    fn update(&self, profile: ProfileSpec, expected_revision: Revision) -> PortResult<()>;
}

pub trait PresetCatalog: fmt::Debug + Send + Sync {
    fn resolve(&self, reference: &EnvironmentPresetRef) -> PortResult<EnvironmentPreset>;
}

/// Secret material with deliberately redacted Debug output.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn new(value: impl Into<Vec<u8>>) -> PortResult<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "secret cannot be empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

pub trait SecretStore: fmt::Debug + Send + Sync {
    fn put(&self, reference: &SecretRef, value: &SecretValue) -> PortResult<()>;
    fn get(&self, reference: &SecretRef) -> PortResult<SecretValue>;
    fn delete(&self, reference: &SecretRef) -> PortResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserProcess {
    pub session_id: SessionId,
    pub process_id: u32,
    pub started_at_ms: u64,
}

pub trait BrowserRuntime: fmt::Debug + Send + Sync {
    fn resolve_build(&self, selector: &BrowserBuildSelector) -> PortResult<BrowserBuild>;
    fn launch(&self, plan: &ResolvedLaunchPlan) -> PortResult<BrowserProcess>;
    fn stop(&self, session_id: &SessionId) -> PortResult<()>;
    fn is_running(&self, session_id: &SessionId) -> PortResult<bool>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyLease {
    pub session_id: SessionId,
    pub proxy_id: ProxyId,
    pub binding: ProxyLaunchBinding,
    /// Opaque adapter-issued handle; never contains a credential.
    pub lease_token: String,
}

pub trait ProxyRuntime: fmt::Debug + Send + Sync {
    fn resolve_proxy(&self, id: &ProxyId) -> PortResult<ProxySpec>;
    fn prepare_session(&self, session_id: &SessionId, proxy: &ProxySpec) -> PortResult<ProxyLease>;
    fn release_session(&self, lease: &ProxyLease) -> PortResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FingerprintContribution {
    pub arguments: Vec<LaunchArgument>,
    pub warnings: Vec<LaunchWarning>,
}

pub trait FingerprintRuntime: fmt::Debug + Send + Sync {
    fn compile(
        &self,
        persona: &FingerprintPersona,
        build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRequest {
    pub profile: ProfileSpec,
    pub session_id: Option<SessionId>,
    pub include_external_checks: bool,
}

pub trait DiagnosticRunner: fmt::Debug + Send + Sync {
    fn run(&self, request: &DiagnosticRequest) -> PortResult<DiagnosticReport>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInspection {
    pub format_version: u16,
    pub profile_name: String,
    pub includes_proxy_credential: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupExportSpec {
    pub destination_name: String,
    pub profile: ProfileSpec,
    pub password: SecretValue,
    pub include_proxy_credential: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupImportSpec {
    pub source_name: String,
    pub password: SecretValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupImportPayload {
    pub profile: ProfileSpec,
    pub restored_data_dir: PathBuf,
}

pub trait BackupService: fmt::Debug + Send + Sync {
    fn inspect(&self, source_name: &str) -> PortResult<BackupInspection>;
    fn export(&self, spec: &BackupExportSpec) -> PortResult<()>;
    fn import(&self, spec: &BackupImportSpec) -> PortResult<BackupImportPayload>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegacyProfileDataSummary {
    pub source_profile_id: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub has_history: bool,
    pub has_cookies: bool,
    pub has_login_data: bool,
    pub has_local_storage: bool,
    pub has_indexed_db: bool,
    pub source_may_be_running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyMigrationArtifact {
    /// Adapter-issued relative backup key. It must never contain an absolute path.
    pub backup_key: String,
    pub copied_files: u64,
    pub copied_bytes: u64,
}

pub trait LegacyProfileDataMigrator: fmt::Debug + Send + Sync {
    fn inspect(
        &self,
        profile_id: &ProfileId,
        source_profile_id: &str,
    ) -> PortResult<LegacyProfileDataSummary>;
    fn migrate(
        &self,
        profile_id: &ProfileId,
        source_profile_id: &str,
        operation_key: &str,
    ) -> PortResult<LegacyMigrationArtifact>;
    fn rollback(&self, profile_id: &ProfileId, backup_key: &str) -> PortResult<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationRecordStatus {
    Running,
    Completed,
    Failed,
    RolledBack,
}

impl MigrationRecordStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRecord {
    pub id: u64,
    pub profile_id: ProfileId,
    pub source_kind: String,
    pub source_profile_id: String,
    pub status: MigrationRecordStatus,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub copied_files: Option<u64>,
    pub copied_bytes: Option<u64>,
    pub backup_key: Option<String>,
    pub error_summary: Option<String>,
}

pub trait MigrationRecordRepository: fmt::Debug + Send + Sync {
    fn begin(
        &self,
        profile_id: &ProfileId,
        source_kind: &str,
        source_profile_id: &str,
        started_at_ms: u64,
    ) -> PortResult<MigrationRecord>;
    fn complete(
        &self,
        id: u64,
        completed_at_ms: u64,
        artifact: &LegacyMigrationArtifact,
    ) -> PortResult<MigrationRecord>;
    fn fail(&self, id: u64, completed_at_ms: u64, summary: &str) -> PortResult<MigrationRecord>;
    fn mark_rolled_back(&self, id: u64, completed_at_ms: u64) -> PortResult<MigrationRecord>;
    fn get(&self, id: u64) -> PortResult<MigrationRecord>;
    fn list(&self, limit: usize) -> PortResult<Vec<MigrationRecord>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub at_ms: u64,
    pub action: String,
    pub entity_id: String,
    pub outcome: AuditOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Succeeded,
    Failed,
}

pub trait AuditSink: fmt::Debug + Send + Sync {
    fn record(&self, event: AuditEvent) -> PortResult<()>;
}

pub trait Clock: fmt::Debug + Send + Sync {
    fn now_ms(&self) -> u64;
}

pub trait IdGenerator: fmt::Debug + Send + Sync {
    fn next_profile_id(&self) -> PortResult<ProfileId>;
    fn next_session_id(&self) -> PortResult<SessionId>;
    fn next_secret_ref(&self) -> PortResult<SecretRef>;
}
