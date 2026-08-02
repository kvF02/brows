//! Deterministic in-memory adapters for unit tests, prototypes, and contract
//! tests. They are not intended to persist production secrets or profiles.

use crate::ports::*;
use crate::sha256::digest_hex;
use brows_domain::{
    BrowserBuild, BrowserBuildId, BrowserBuildSelector, BrowserChannel, DiagnosticCheck,
    DiagnosticOverall, DiagnosticReport, DiagnosticScope, DiagnosticStatus, EnvironmentPreset,
    EnvironmentPresetRef, FingerprintPersona, LaunchWarning, PresetId, ProfileId, ProfileSpec,
    ProxyId, ProxyLaunchBinding, ProxySpec, ResolvedLaunchPlan, Revision, SecretRef, SessionId,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

fn unavailable(component: &'static str) -> PortError {
    PortError::new(
        PortErrorKind::Unavailable,
        format!("{component} state is unavailable"),
    )
}

#[derive(Debug, Default)]
pub struct InMemoryProfileRepository {
    values: RwLock<BTreeMap<ProfileId, ProfileSpec>>,
}

impl InMemoryProfileRepository {
    pub fn profiles(&self) -> PortResult<Vec<ProfileSpec>> {
        Ok(self
            .values
            .read()
            .map_err(|_| unavailable("profile repository"))?
            .values()
            .cloned()
            .collect())
    }
}

impl ProfileRepository for InMemoryProfileRepository {
    fn list(&self, query: &ProfileQuery) -> PortResult<Vec<ProfileSpec>> {
        let search = query
            .search
            .as_ref()
            .map(|value| value.to_ascii_lowercase());
        let values = self
            .values
            .read()
            .map_err(|_| unavailable("profile repository"))?;
        Ok(values
            .values()
            .filter(|profile| query.include_archived || !profile.archived)
            .filter(|profile| {
                search.as_ref().is_none_or(|needle| {
                    profile.name.to_ascii_lowercase().contains(needle)
                        || profile
                            .tags
                            .iter()
                            .any(|tag| tag.to_ascii_lowercase().contains(needle))
                })
            })
            .skip(query.offset)
            .take(query.limit)
            .cloned()
            .collect())
    }

    fn get(&self, id: &ProfileId) -> PortResult<ProfileSpec> {
        self.values
            .read()
            .map_err(|_| unavailable("profile repository"))?
            .get(id)
            .cloned()
            .ok_or_else(|| PortError::not_found("profile"))
    }

    fn create(&self, profile: ProfileSpec) -> PortResult<()> {
        profile
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let mut values = self
            .values
            .write()
            .map_err(|_| unavailable("profile repository"))?;
        if values.contains_key(&profile.id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "profile already exists",
            ));
        }
        values.insert(profile.id.clone(), profile);
        Ok(())
    }

    fn update(&self, profile: ProfileSpec, expected_revision: Revision) -> PortResult<()> {
        profile
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        let mut values = self
            .values
            .write()
            .map_err(|_| unavailable("profile repository"))?;
        let current = values
            .get(&profile.id)
            .ok_or_else(|| PortError::not_found("profile"))?;
        if current.revision != expected_revision {
            return Err(PortError::new(
                PortErrorKind::RevisionConflict,
                "profile revision changed",
            ));
        }
        if profile.revision != expected_revision.next() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "replacement revision must increment exactly once",
            ));
        }
        values.insert(profile.id.clone(), profile);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct InMemoryPresetCatalog {
    values: RwLock<BTreeMap<(PresetId, u32), EnvironmentPreset>>,
}

impl InMemoryPresetCatalog {
    pub fn insert(&self, preset: EnvironmentPreset) -> PortResult<()> {
        preset
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.values
            .write()
            .map_err(|_| unavailable("preset catalog"))?
            .insert(
                (preset.reference.id.clone(), preset.reference.version),
                preset,
            );
        Ok(())
    }
}

impl PresetCatalog for InMemoryPresetCatalog {
    fn resolve(&self, reference: &EnvironmentPresetRef) -> PortResult<EnvironmentPreset> {
        self.values
            .read()
            .map_err(|_| unavailable("preset catalog"))?
            .get(&(reference.id.clone(), reference.version))
            .cloned()
            .ok_or_else(|| PortError::not_found("environment preset"))
    }
}

#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    values: RwLock<BTreeMap<SecretRef, SecretValue>>,
}

impl SecretStore for InMemorySecretStore {
    fn put(&self, reference: &SecretRef, value: &SecretValue) -> PortResult<()> {
        self.values
            .write()
            .map_err(|_| unavailable("secret store"))?
            .insert(reference.clone(), value.clone());
        Ok(())
    }

    fn get(&self, reference: &SecretRef) -> PortResult<SecretValue> {
        self.values
            .read()
            .map_err(|_| unavailable("secret store"))?
            .get(reference)
            .cloned()
            .ok_or_else(|| PortError::not_found("secret"))
    }

    fn delete(&self, reference: &SecretRef) -> PortResult<()> {
        self.values
            .write()
            .map_err(|_| unavailable("secret store"))?
            .remove(reference)
            .map(|_| ())
            .ok_or_else(|| PortError::not_found("secret"))
    }
}

#[derive(Debug)]
pub struct InMemoryBrowserRuntime {
    builds: RwLock<BTreeMap<BrowserBuildId, BrowserBuild>>,
    running: Mutex<BTreeMap<SessionId, (BrowserProcess, ResolvedLaunchPlan)>>,
    next_process_id: AtomicU32,
    fail_next_launch: AtomicBool,
    clock: Arc<dyn Clock>,
}

impl InMemoryBrowserRuntime {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            builds: RwLock::new(BTreeMap::new()),
            running: Mutex::new(BTreeMap::new()),
            next_process_id: AtomicU32::new(10_000),
            fail_next_launch: AtomicBool::new(false),
            clock,
        }
    }

    pub fn insert_build(&self, build: BrowserBuild) -> PortResult<()> {
        build
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.builds
            .write()
            .map_err(|_| unavailable("browser runtime"))?
            .insert(build.id.clone(), build);
        Ok(())
    }

    pub fn fail_next_launch(&self) {
        self.fail_next_launch.store(true, Ordering::SeqCst);
    }

    pub fn running_plans(&self) -> PortResult<Vec<ResolvedLaunchPlan>> {
        Ok(self
            .running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .values()
            .map(|(_, plan)| plan.clone())
            .collect())
    }
}

impl BrowserRuntime for InMemoryBrowserRuntime {
    fn resolve_build(&self, selector: &BrowserBuildSelector) -> PortResult<BrowserBuild> {
        let builds = self
            .builds
            .read()
            .map_err(|_| unavailable("browser runtime"))?;
        match selector {
            BrowserBuildSelector::Pinned(id) => builds
                .get(id)
                .cloned()
                .ok_or_else(|| PortError::not_found("browser build")),
            BrowserBuildSelector::Channel(channel) => builds
                .values()
                .filter(|build| &build.channel == channel)
                .max_by_key(|build| build.installed_at_ms)
                .cloned()
                .ok_or_else(|| PortError::not_found("browser build")),
        }
    }

    fn launch(&self, plan: &ResolvedLaunchPlan) -> PortResult<BrowserProcess> {
        plan.validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        if self.fail_next_launch.swap(false, Ordering::SeqCst) {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "simulated browser launch failure",
            ));
        }
        let mut running = self
            .running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?;
        if running.contains_key(&plan.session_id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "browser session is already running",
            ));
        }
        let process = BrowserProcess {
            session_id: plan.session_id.clone(),
            process_id: self.next_process_id.fetch_add(1, Ordering::SeqCst),
            started_at_ms: self.clock.now_ms(),
        };
        running.insert(plan.session_id.clone(), (process.clone(), plan.clone()));
        Ok(process)
    }

    fn stop(&self, session_id: &SessionId) -> PortResult<()> {
        self.running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .remove(session_id)
            .map(|_| ())
            .ok_or_else(|| PortError::not_found("running browser session"))
    }

    fn is_running(&self, session_id: &SessionId) -> PortResult<bool> {
        Ok(self
            .running
            .lock()
            .map_err(|_| unavailable("browser runtime"))?
            .contains_key(session_id))
    }
}

#[derive(Debug, Default)]
pub struct InMemoryProxyRuntime {
    proxies: RwLock<BTreeMap<ProxyId, ProxySpec>>,
    leases: Mutex<BTreeMap<SessionId, ProxyLease>>,
    next_port: AtomicU32,
}

impl InMemoryProxyRuntime {
    pub fn insert_proxy(&self, proxy: ProxySpec) -> PortResult<()> {
        proxy
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.proxies
            .write()
            .map_err(|_| unavailable("proxy runtime"))?
            .insert(proxy.id.clone(), proxy);
        Ok(())
    }

    pub fn active_leases(&self) -> PortResult<Vec<ProxyLease>> {
        Ok(self
            .leases
            .lock()
            .map_err(|_| unavailable("proxy runtime"))?
            .values()
            .cloned()
            .collect())
    }
}

impl ProxyRuntime for InMemoryProxyRuntime {
    fn resolve_proxy(&self, id: &ProxyId) -> PortResult<ProxySpec> {
        self.proxies
            .read()
            .map_err(|_| unavailable("proxy runtime"))?
            .get(id)
            .cloned()
            .ok_or_else(|| PortError::not_found("proxy"))
    }

    fn prepare_session(&self, session_id: &SessionId, proxy: &ProxySpec) -> PortResult<ProxyLease> {
        proxy
            .validate()
            .map_err(|error| PortError::new(PortErrorKind::InvalidData, error.to_string()))?;
        self.resolve_proxy(&proxy.id)?;
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| unavailable("proxy runtime"))?;
        if leases.contains_key(session_id) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "proxy lease already exists for the session",
            ));
        }
        let sequence = self.next_port.fetch_add(1, Ordering::SeqCst);
        let port = 20_000 + (sequence % 30_000) as u16;
        let lease = ProxyLease {
            session_id: session_id.clone(),
            proxy_id: proxy.id.clone(),
            binding: ProxyLaunchBinding::Loopback {
                host: "127.0.0.1".to_owned(),
                port,
            },
            lease_token: format!("lease-{sequence}"),
        };
        leases.insert(session_id.clone(), lease.clone());
        Ok(lease)
    }

    fn release_session(&self, lease: &ProxyLease) -> PortResult<()> {
        let mut leases = self
            .leases
            .lock()
            .map_err(|_| unavailable("proxy runtime"))?;
        let current = leases
            .get(&lease.session_id)
            .ok_or_else(|| PortError::not_found("proxy lease"))?;
        if current.lease_token != lease.lease_token {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "proxy lease token does not match",
            ));
        }
        leases.remove(&lease.session_id);
        Ok(())
    }
}

impl ProxyRepository for InMemoryProxyRuntime {
    fn list(&self) -> PortResult<Vec<ProxySpec>> {
        Ok(self
            .proxies
            .read()
            .map_err(|_| unavailable("proxy repository"))?
            .values()
            .cloned()
            .collect())
    }

    fn get(&self, id: &ProxyId) -> PortResult<ProxySpec> {
        self.resolve_proxy(id)
    }

    fn save(&self, proxy: &ProxySpec) -> PortResult<()> {
        self.insert_proxy(proxy.clone())
    }

    fn delete(&self, id: &ProxyId) -> PortResult<()> {
        self.proxies
            .write()
            .map_err(|_| unavailable("proxy repository"))?
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| PortError::not_found("proxy"))
    }
}

#[derive(Debug, Default)]
pub struct InMemoryProxyConnectivityTester;

impl ProxyConnectivityTester for InMemoryProxyConnectivityTester {
    fn test(
        &self,
        _proxy: &ProxySpec,
        _credential: Option<&SecretValue>,
    ) -> PortResult<ProxyConnectivityCheck> {
        Ok(ProxyConnectivityCheck {
            reachable: true,
            latency_ms: 1,
            detail: "in-memory endpoint reachable".to_owned(),
            exit_ip: Some("203.0.113.1".to_owned()),
            exit_ip_checked: true,
        })
    }
}

#[derive(Debug, Default)]
pub struct SystemNativeFingerprintRuntime;

impl FingerprintRuntime for SystemNativeFingerprintRuntime {
    fn compile(
        &self,
        persona: &FingerprintPersona,
        _build: &BrowserBuild,
    ) -> PortResult<FingerprintContribution> {
        let warnings = persona
            .host_shared_fields()
            .iter()
            .map(|field| LaunchWarning::HostFingerprintShared(field))
            .collect();
        Ok(FingerprintContribution {
            arguments: Vec::new(),
            warnings,
        })
    }
}

#[derive(Debug)]
pub struct InMemoryDiagnosticRunner {
    reports: Mutex<Vec<DiagnosticReport>>,
    clock: Arc<dyn Clock>,
}

impl InMemoryDiagnosticRunner {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            reports: Mutex::new(Vec::new()),
            clock,
        }
    }

    pub fn reports(&self) -> PortResult<Vec<DiagnosticReport>> {
        Ok(self
            .reports
            .lock()
            .map_err(|_| unavailable("diagnostic runner"))?
            .clone())
    }
}

impl DiagnosticRunner for InMemoryDiagnosticRunner {
    fn run(&self, request: &DiagnosticRequest) -> PortResult<DiagnosticReport> {
        let mut checks = vec![DiagnosticCheck {
            key: "profile_storage".to_owned(),
            scope: DiagnosticScope::Isolated,
            status: DiagnosticStatus::Pass,
            observed: Some("dedicated user data directory".to_owned()),
            expected: Some("isolated".to_owned()),
            explanation: "site storage is scoped to this profile".to_owned(),
        }];
        checks.extend(
            request
                .profile
                .persona
                .host_shared_fields()
                .iter()
                .map(|field| DiagnosticCheck {
                    key: (*field).to_owned(),
                    scope: DiagnosticScope::HostShared,
                    status: DiagnosticStatus::Warn,
                    observed: Some("system native".to_owned()),
                    expected: Some("system native".to_owned()),
                    explanation: "v0.1 intentionally shares this host property".to_owned(),
                }),
        );
        let report = DiagnosticReport {
            profile_id: request.profile.id.clone(),
            session_id: request.session_id.clone(),
            generated_at_ms: self.clock.now_ms(),
            checks,
            overall: DiagnosticOverall::Attention,
            external_endpoints: if request.include_external_checks {
                vec!["https://diagnostic.invalid/explicit-test".to_owned()]
            } else {
                Vec::new()
            },
        };
        self.reports
            .lock()
            .map_err(|_| unavailable("diagnostic runner"))?
            .push(report.clone());
        Ok(report)
    }
}

#[derive(Debug, Clone)]
struct StoredBackup {
    profile: ProfileSpec,
    password: SecretValue,
    includes_proxy_credential: bool,
}

#[derive(Debug, Default)]
pub struct InMemoryBackupService {
    backups: RwLock<BTreeMap<String, StoredBackup>>,
}

impl BackupService for InMemoryBackupService {
    fn inspect(&self, source_name: &str) -> PortResult<BackupInspection> {
        let backups = self
            .backups
            .read()
            .map_err(|_| unavailable("backup service"))?;
        let backup = backups
            .get(source_name)
            .ok_or_else(|| PortError::not_found("backup"))?;
        Ok(BackupInspection {
            format_version: 1,
            profile_name: backup.profile.name.clone(),
            includes_proxy_credential: backup.includes_proxy_credential,
        })
    }

    fn export(&self, spec: &BackupExportSpec) -> PortResult<()> {
        if spec.destination_name.trim().is_empty() {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "backup destination name cannot be empty",
            ));
        }
        let mut backups = self
            .backups
            .write()
            .map_err(|_| unavailable("backup service"))?;
        if backups.contains_key(&spec.destination_name) {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "backup destination already exists",
            ));
        }
        backups.insert(
            spec.destination_name.clone(),
            StoredBackup {
                profile: spec.profile.clone(),
                password: spec.password.clone(),
                includes_proxy_credential: spec.include_proxy_credential,
            },
        );
        Ok(())
    }

    fn import(&self, spec: &BackupImportSpec) -> PortResult<BackupImportPayload> {
        let backups = self
            .backups
            .read()
            .map_err(|_| unavailable("backup service"))?;
        let backup = backups
            .get(&spec.source_name)
            .ok_or_else(|| PortError::not_found("backup"))?;
        if backup.password != spec.password {
            return Err(PortError::new(
                PortErrorKind::WrongPassword,
                "backup password is incorrect",
            ));
        }
        Ok(BackupImportPayload {
            profile: backup.profile.clone(),
            restored_data_dir: PathBuf::from("memory-backups")
                .join(digest_hex(spec.source_name.as_bytes())),
        })
    }
}

#[derive(Debug, Default)]
pub struct InMemoryAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl InMemoryAuditSink {
    pub fn events(&self) -> PortResult<Vec<AuditEvent>> {
        Ok(self
            .events
            .lock()
            .map_err(|_| unavailable("audit sink"))?
            .clone())
    }
}

impl AuditSink for InMemoryAuditSink {
    fn record(&self, event: AuditEvent) -> PortResult<()> {
        self.events
            .lock()
            .map_err(|_| unavailable("audit sink"))?
            .push(event);
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ManualClock {
    now_ms: AtomicU64,
}

impl ManualClock {
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    pub fn set(&self, now_ms: u64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, delta_ms: u64) -> u64 {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst) + delta_ms
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Default)]
pub struct SequentialIdGenerator {
    profile: AtomicU64,
    proxy: AtomicU64,
    session: AtomicU64,
    secret: AtomicU64,
}

impl IdGenerator for SequentialIdGenerator {
    fn next_profile_id(&self) -> PortResult<ProfileId> {
        ProfileId::new(format!(
            "profile-{}",
            self.profile.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }

    fn next_proxy_id(&self) -> PortResult<ProxyId> {
        let value = self.proxy.fetch_add(1, Ordering::SeqCst) + 1;
        ProxyId::new(format!("proxy-{value}"))
            .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }

    fn next_session_id(&self) -> PortResult<SessionId> {
        SessionId::new(format!(
            "session-{}",
            self.session.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }

    fn next_secret_ref(&self) -> PortResult<SecretRef> {
        SecretRef::new(format!(
            "secret-{}",
            self.secret.fetch_add(1, Ordering::SeqCst) + 1
        ))
        .map_err(|error| PortError::new(PortErrorKind::Internal, error.to_string()))
    }
}

/// Convenience bootstrap for the single v0.1 preset.
pub fn system_native_preset() -> EnvironmentPreset {
    use brows_domain::EnvironmentPresetKind;
    EnvironmentPreset {
        reference: EnvironmentPresetRef::new(
            PresetId::new("system-native").expect("static preset id is valid"),
            1,
        )
        .expect("static preset version is valid"),
        name: "System Native".to_owned(),
        kind: EnvironmentPresetKind::SystemNative,
        description: "Use truthful host-level browser properties".to_owned(),
    }
}

/// Convenience helper for resolving the newest stable build in adapter tests.
pub fn stable_selector() -> BrowserBuildSelector {
    BrowserBuildSelector::Channel(BrowserChannel::Stable)
}
