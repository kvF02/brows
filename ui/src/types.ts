/**
 * Transport and domain-facing types used by the UI.
 *
 * These types deliberately describe capabilities rather than implementation
 * details.  The Rust contracts can generate an equivalent declaration later;
 * keeping the UI against this small interface prevents it from depending on
 * Tauri, SQLite, or the Chromium process directly.
 */

export type ProfileStatus =
  | "stopped"
  | "queued"
  | "starting"
  | "running"
  | "stopping"
  | "failed"
  | "archived";

export type ProxyKind = "http" | "https" | "socks5";
export type ProxyHealth = "unknown" | "checking" | "healthy" | "unhealthy";
export type FingerprintScope = "isolated" | "configurable" | "host-shared";

export interface ProxySummary {
  id: string;
  name: string;
  kind: ProxyKind;
  host: string;
  port: number;
  authenticated: boolean;
  health: ProxyHealth;
  lastCheckedAt?: string;
  lastExitIp?: string;
  exitIpChecked?: boolean;
}

export interface ProxyInput {
  name: string;
  kind: ProxyKind;
  host: string;
  port: number;
  username?: string;
  /** Transient input. The desktop adapter stores it in the OS vault and never returns it. */
  credential?: string;
}

export interface EnvironmentPreset {
  id: string;
  name: string;
  description: string;
  locale: string;
  timezonePolicy: "system" | "proxy-region" | "explicit";
  fingerprintScope: FingerprintScope;
}

export interface BrowserRuntime {
  id: string;
  name: string;
  version: string;
  channel: "stable" | "beta" | "dev" | "custom";
  trusted: boolean;
  active: boolean;
  /** Signature trust and MAIN-world fingerprint extension support are separate checks. */
  fingerprintSupport: "unverified" | "supported" | "unsupported";
  sha256?: string;
}

export interface RuntimeFingerprintProbeResult {
  runtime: BrowserRuntime;
  detail: string;
}

export interface DiagnosticSummary {
  overall: "not-run" | "pass" | "warning" | "fail";
  checkedAt?: string;
  issueCount: number;
}

export interface Profile {
  id: string;
  name: string;
  tags: string[];
  presetId: string;
  proxyId?: string;
  runtimeId: string;
  status: ProfileStatus;
  revision: number;
  createdAt: string;
  lastUsedAt?: string;
  diagnostic: DiagnosticSummary;
  persona?: PersonaConfig;
  acceptLanguages?: string[];
  windowWidth?: number;
  windowHeight?: number;
}

export interface ProfileInput {
  name: string;
  tags: string[];
  presetId: string;
  proxyId?: string;
  runtimeId: string;
  /** v0.2: optional SeedableNoise persona. If omitted, SystemNative is used. */
  persona?: PersonaConfig;
  acceptLanguages?: string[];
  windowWidth?: number;
  windowHeight?: number;
}

// ── v0.2 Fingerprint Persona (SeedableNoise) ────────────────────────────────

export type PersonaKind = "system_native" | "seedable_noise";

export type PlatformKind = "windows" | "mac_os" | "linux";

export type DeviceMemoryGb = "0.25" | "0.5" | "1" | "2" | "4" | "8";

export interface SeedableNoisePersonaConfig {
  seed: string;
  platform: PlatformKind;
  hardwareConcurrency: number;
  deviceMemory: DeviceMemoryGb;
  touchPoints: number;
  screenWidth: number;
  screenHeight: number;
  colorDepth: number;
  deviceScaleFactor: number; // multiplier, e.g. 1.0, 1.5, 2.0
  webglVendor: string;       // empty = auto-derive
  webglRenderer: string;     // empty = auto-derive
  spoofCanvas: boolean;
  spoofAudio: boolean;
}

export interface PersonaConfig {
  kind: PersonaKind;
  seedable?: SeedableNoisePersonaConfig;
}

export interface FingerprintPreview {
  runtimeId: string;
  runtimeVersion: string;
  effectiveUserAgent: string;
  effectivePlatform: string;
  effectiveLanguages: string[];
  effectiveWindowWidth: number;
  effectiveWindowHeight: number;
  effectiveScreenWidth: number;
  effectiveScreenHeight: number;
  deviceScaleFactorHundredths: number;
  effectiveWebglVendor: string;
  effectiveWebglRenderer: string;
  hardwareConcurrency: number;
  deviceMemoryGb: string;
  declaredOnlyFields: string[];
  hostSharedFields: string[];
  runtimeWarnings: string[];
}

export interface FingerprintPreviewInput {
  persona: PersonaConfig;
  runtimeId: string;
  acceptLanguages: string[];
  windowWidth: number;
  windowHeight: number;
}

export interface MoreLoginCandidate {
  rowNumber: number;
  sourceProfileId: string;
  name: string;
  tags: string[];
  hasProxy: boolean;
  hasLoginData: boolean;
  hasCookie: boolean;
  hasCustomUa: boolean;
  warnings: string[];
}

export interface MoreLoginPreview {
  sourceName: string;
  profileCount: number;
  sensitiveProfileCount: number;
  candidates: MoreLoginCandidate[];
  notices: string[];
}

export interface MoreLoginImportResult {
  importedCount: number;
  skippedCount: number;
  failures: Array<{ rowNumber: number; reason: string }>;
}

export interface LegacyMigrationInspection {
  sourceProfileId: string;
  fileCount: number;
  totalBytes: number;
  hasHistory: boolean;
  hasCookies: boolean;
  hasLoginData: boolean;
  hasLocalStorage: boolean;
  hasIndexedDb: boolean;
  sourceMayBeRunning: boolean;
}

export type LegacyMigrationStatus = "running" | "completed" | "failed" | "rolled_back";

export interface LegacyMigrationRecord {
  id: number;
  profileId: string;
  sourceKind: string;
  sourceProfileId: string;
  status: LegacyMigrationStatus;
  startedAtMs: number;
  completedAtMs?: number;
  copiedFiles?: number;
  copiedBytes?: number;
  canRollback: boolean;
  errorSummary?: string;
}

export interface SessionSummary {
  id: string;
  profileId: string;
  state: Exclude<ProfileStatus, "archived">;
  createdAtMs: number;
  updatedAtMs: number;
  failureCode?: string;
}

export interface DiagnosticItem {
  key: string;
  label: string;
  actual: string;
  expected?: string;
  scope: FingerprintScope;
  status: "pass" | "warning" | "fail" | "not-checked";
  explanation: string;
}

export interface DiagnosticReport {
  profileId: string;
  checkedAt: string;
  overall: "pass" | "warning" | "fail";
  items: DiagnosticItem[];
  externalEndpoints: string[];
}

export type BackupStatus = "ready" | "running" | "completed" | "failed" | "interrupted" | "cancelled";

export type BackupPhase = "preparing" | "deriving_key" | "processing" | "verifying" | "committing" | "cleaning" | "completed" | "cancelled" | "failed";

export interface BackupJob {
  id: string;
  profileId: string;
  profileName: string;
  status: BackupStatus;
  operationId?: string;
  phase?: BackupPhase;
  processedBytes?: number;
  totalBytes?: number;
  canCancel?: boolean;
  cancelRequested?: boolean;
  createdAt: string;
  sizeBytes?: number;
  note?: string;
}

export interface Settings {
  maxConcurrentSessions: number;
  telemetryEnabled: false;
  stopSessionsOnExit: true;
  retainArchivedDays: number;
  activeRuntimeId?: string;
}

export interface CommandMap {
  "profile.list": { params: { includeArchived?: boolean }; result: Profile[] };
  "profile.get": { params: { id: string }; result: Profile };
  "profile.create": { params: ProfileInput; result: Profile };
  "profile.update": {
    params: { id: string; revision: number; changes: Partial<ProfileInput> };
    result: Profile;
  };
  "profile.clone": { params: { id: string }; result: Profile };
  "profile.archive": { params: { id: string; revision: number }; result: Profile };
  "profile.restore": { params: { id: string; revision: number }; result: Profile };
  "session.launch": { params: { profileId: string }; result: Profile };
  "session.stop": { params: { profileId: string }; result: Profile };
  "session.cancel_queued": { params: { sessionId: string }; result: SessionSummary };
  "proxy.list": { params: Record<string, never>; result: ProxySummary[] };
  "proxy.save": { params: ProxyInput; result: ProxySummary };
  "proxy.test": { params: { id: string }; result: ProxySummary };
  "diagnostic.list": { params: { profileId?: string }; result: DiagnosticReport[] };
  "diagnostic.run": { params: { profileId: string }; result: DiagnosticReport };
  "backup.list": { params: Record<string, never>; result: BackupJob[] };
  "backup.export": {
    params: { profileId: string; destinationName: string; includeProxySecret: boolean; password: string };
    result: BackupJob;
  };
  "backup.import": { params: { sourceName: string; password: string }; result: Profile };
  "runtime.list": { params: Record<string, never>; result: BrowserRuntime[] };
  "settings.get": { params: Record<string, never>; result: Settings };
  "settings.update": { params: Partial<Settings>; result: Settings };
}

export type CommandName = keyof CommandMap;
export type CommandParams<Name extends CommandName> = CommandMap[Name]["params"];
export type CommandResult<Name extends CommandName> = CommandMap[Name]["result"];

export interface ApiClient {
  /** Derive a draft configuration for the selected runtime; this is not a browser observation. */
  fingerprintPreview(input: FingerprintPreviewInput): Promise<FingerprintPreview>;
  /** Launch an isolated local probe to check extension injection; not an anti-detect guarantee. */
  probeRuntimeFingerprint(runtimeId: string): Promise<RuntimeFingerprintProbeResult>;
  /** List registered browser runtimes (auto-detected from the local system). */
  listRuntimes(): Promise<BrowserRuntime[]>;
  previewMoreLogin(sourcePath: string): Promise<MoreLoginPreview>;
  importMoreLogin(sourcePath: string, rowNumbers?: number[]): Promise<MoreLoginImportResult>;
  inspectLegacyMigration(profileId: string, sourceProfileId: string): Promise<LegacyMigrationInspection>;
  executeLegacyMigration(profileId: string, sourceProfileId: string): Promise<LegacyMigrationRecord>;
  rollbackLegacyMigration(migrationId: number): Promise<LegacyMigrationRecord>;
  listLegacyMigrations(limit?: number): Promise<LegacyMigrationRecord[]>;
  listProfiles(includeArchived?: boolean): Promise<Profile[]>;
  getProfile(id: string): Promise<Profile>;
  createProfile(input: ProfileInput): Promise<Profile>;
  updateProfile(
    id: string,
    revision: number,
    changes: Partial<ProfileInput>,
  ): Promise<Profile>;
  cloneProfile(id: string): Promise<Profile>;
  archiveProfile(id: string, revision: number): Promise<Profile>;
  restoreProfile(id: string, revision: number): Promise<Profile>;
  launchSession(profileId: string): Promise<Profile>;
  stopSession(profileId: string): Promise<Profile>;
  cancelQueuedSession(sessionId: string): Promise<SessionSummary>;
  listProxies(): Promise<ProxySummary[]>;
  saveProxy(input: ProxyInput): Promise<ProxySummary>;
  testProxy(id: string): Promise<ProxySummary>;
  removeProxy(id: string): Promise<void>;
  listDiagnostics(profileId?: string): Promise<DiagnosticReport[]>;
  runDiagnostic(profileId: string, external?: boolean): Promise<DiagnosticReport>;
  listBackups(): Promise<BackupJob[]>;
  cancelBackup(operationId: string): Promise<void>;
  cleanupBackupStaging(): Promise<{ cleanupPending: boolean }>;
  exportBackup(
    profileId: string,
    destinationName: string,
    includeProxySecret: boolean,
    password: string,
  ): Promise<BackupJob>;
  importBackup(sourceName: string, password: string): Promise<Profile>;
  listRuntimes(): Promise<BrowserRuntime[]>;
  getSettings(): Promise<Settings>;
  updateSettings(changes: Partial<Settings>): Promise<Settings>;
}

export class ApiError extends Error {
  readonly code: string;
  readonly details?: unknown;

  constructor(code: string, message: string, details?: unknown) {
    super(message);
    this.name = "ApiError";
    this.code = code;
    this.details = details;
  }
}

export const DEFAULT_PRESETS: EnvironmentPreset[] = [
  {
    id: "system-native",
    name: "系统原生",
    description: "使用真实 Windows 平台属性，不修改底层设备指纹",
    locale: "zh-CN",
    timezonePolicy: "system",
    fingerprintScope: "host-shared",
  },
];

export const EMPTY_SETTINGS: Settings = {
  maxConcurrentSessions: 10,
  telemetryEnabled: false,
  stopSessionsOnExit: true,
  retainArchivedDays: 7,
};
