import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import {
  ApiError,
  type ApiClient,
  type BackupJob,
  type BrowserRuntime,
  type DiagnosticReport,
  type FingerprintPreview,
  type FingerprintPreviewInput,
  type LegacyMigrationInspection,
  type LegacyMigrationRecord,
  type MoreLoginImportResult,
  type MoreLoginPreview,
  type Profile,
  type ProfileInput,
  type ProfileStatus,
  type PersonaConfig,
  type ProxyInput,
  type ProxySummary,
  type RuntimeFingerprintProbeResult,
  type SessionSummary,
  type SessionObservation,
  type NetworkProbeService,
  type NetworkProbeObservation,
  type Settings,
} from "../types";

export interface TypedCommandBridge {
  invoke(handler: string, args?: Record<string, unknown>): Promise<unknown>;
}

type BrowserChannelDto = "stable" | "beta" | "dev" | "canary";

type BrowserSelectorDto =
  | { mode: "pinned"; value: { buildId: string } }
  | { mode: "channel"; value: { channel: BrowserChannelDto } };

interface ProfileDraftDto {
  name: string;
  tags: string[];
  presetId: string;
  presetVersion: number;
  proxyId?: string;
  browser: BrowserSelectorDto;
  persona: {
    system_native: {
      preserveSystemTimezone: true;
      preserveSystemFonts: true;
      preserveSystemGpu: true;
      preserveSystemAudio: true;
    };
  } | {
    seedable_noise: {
      seed: string;
      platform: "windows" | "mac_os" | "linux";
      hardwareConcurrency: number;
      deviceMemory: "quarter" | "half" | "one" | "two" | "four" | "eight";
      touchPoints: number;
      screen: {
        width: number;
        height: number;
        colorDepth: number;
        deviceScaleFactorHundredths: number;
      };
      webglVendor: string;
      webglRenderer: string;
      spoofCanvas: boolean;
      spoofAudio: boolean;
    };
  };
  locale: { acceptLanguages: string[]; timezone?: string };
  window: { width: number; height: number; startMaximized: boolean };
  startupUrls: string[];
}

interface ProfileDto extends ProfileDraftDto {
  id: string;
  revision: number;
  archived: boolean;
}

interface SessionDto {
  id: string;
  profileId: string;
  state: Exclude<ProfileStatus, "archived">;
  createdAtMs: number;
  updatedAtMs: number;
  failureCode?: string;
}

interface ErrorDto {
  code?: string;
  message?: string;
  violations?: unknown[];
}

interface ProxyDto {
  id: string;
  name: string;
  scheme: "http" | "https" | "socks5";
  host: string;
  port: number;
  username?: string;
  hasSecret: boolean;
}

interface ProxyConnectivityCheckDto {
  proxyId: string;
  reachable: boolean;
  latencyMs: number;
  detail: string;
  exitIp?: string;
  exitIpChecked: boolean;
}

interface DiagnosticReportDto {
  profileId: string;
  sessionId?: string;
  generatedAtMs: number;
  checks: Array<{ key: string; scope: "isolated" | "configurable" | "host_shared"; status: "pass" | "warn" | "fail" | "not_checked"; observed?: string; expected?: string; explanation: string }>;
  overall: "healthy" | "attention" | "failed";
  externalEndpoints: string[];
}

interface BackupJobDto {
  id: string;
  profileId: string;
  profileName: string;
  status: "ready" | "running" | "completed" | "failed";
  createdAt: string;
  sizeBytes?: number;
  note?: string;
}
interface BackupImportResultDto {
  profile: ProfileDto;
  note: string;
}

const handlers = Object.freeze({
  profileList: "profile_list",
  profileGet: "profile_get",
  profileCreate: "profile_create",
  profileUpdate: "profile_update",
  profileClone: "profile_clone",
  profileArchive: "profile_archive",
  profileRestore: "profile_restore",
  sessionLaunch: "session_launch",
  sessionStop: "session_stop",
  sessionCancelQueued: "session_cancel_queued",
  sessionList: "session_list",
  sessionObserve: "session_observe",
  networkProbeService: "network_probe_service",
  sessionProbeNetwork: "session_probe_network",
  moreLoginPreview: "morelogin_preview",
  moreLoginImport: "morelogin_import",
  proxyList: "proxy_list",
  proxySave: "proxy_save",
  proxyTest: "proxy_test",
  proxyRemove: "proxy_remove",
  diagnosticRun: "diagnostic_run",
  diagnosticList: "diagnostic_list",
  legacyMigrationInspect: "legacy_migration_inspect",
  legacyMigrationExecute: "legacy_migration_execute",
  legacyMigrationRollback: "legacy_migration_rollback",
  legacyMigrationList: "legacy_migration_list",
  fingerprintPreview: "fingerprint_preview",
  listRuntimes: "list_runtimes",
  probeRuntimeFingerprint: "probe_runtime_fingerprint",
  backupList: "backup_list",
  backupExport: "backup_export",
  backupImport: "backup_import",
  settingsGet: "settings_get",
  settingsUpdate: "settings_update",
  runtimeCapture: "runtime_capture",
  runtimeActivate: "runtime_activate",
  supportReport: "support_report",
});

const prototypeRuntime: BrowserRuntime = {
  id: "google-chrome-stable-150",
  name: "Google Chrome",
  version: "150.0.7871.188",
  channel: "stable",
  trusted: true,
  active: true,
  fingerprintSupport: "unverified",
};

export function isTauriRuntime(): boolean {
  if (typeof window === "undefined") return false;
  // Brows sets a custom marker in the Tauri runtime setup so the frontend can
  // detect its environment even when running against an external Vite dev URL.
  return (
    "__BROWS_IS_TAURI__" in window ||
    "__TAURI_INTERNALS__" in window ||
    "__TAURI__" in window ||
    "isTauri" in window
  );
}

export function createTauriBridge(): TypedCommandBridge {
  return {
    invoke(handler: string, args?: Record<string, unknown>): Promise<unknown> {
      return tauriInvoke(handler, args);
    },
  };
}

/** Maps the versioned Rust DTOs to stable UI view models. */
export class TauriApiClient implements ApiClient {
  private readonly dtoCache = new Map<string, ProfileDto>();
  private readonly firstSeen = new Map<string, string>();
  private sessions: SessionDto[] = [];
  private sessionSequence = 0;
  private appliedSessionSequence = 0;
  private readonly proxyHealth = new Map<string, Pick<ProxySummary, "health" | "lastCheckedAt" | "lastExitIp" | "exitIpChecked">>();

  constructor(private readonly bridge: TypedCommandBridge) {}

  async previewMoreLogin(sourcePath: string): Promise<MoreLoginPreview> {
    return this.call<MoreLoginPreview>(handlers.moreLoginPreview, { request: { sourcePath } });
  }

  async importMoreLogin(sourcePath: string, rowNumbers: number[] = []): Promise<MoreLoginImportResult> {
    return this.call<MoreLoginImportResult>(handlers.moreLoginImport, {
      request: { sourcePath, rowNumbers },
    });
  }

  async fingerprintPreview(input: FingerprintPreviewInput): Promise<FingerprintPreview> {
    return this.call<FingerprintPreview>(handlers.fingerprintPreview, {
      request: {
        ...input,
        persona: buildPersonaDto(input.persona),
      },
    });
  }

  async listRuntimes(): Promise<BrowserRuntime[]> {
    // list_runtimes takes no payload; passing { request: undefined } would be
    // rejected by Tauri.
    const raw = (await this.bridge.invoke(handlers.listRuntimes, undefined)) as Array<{
      id: string;
      name: string;
      version: string;
      channel: string;
      trusted: boolean;
      active: boolean;
      fingerprintSupport?: BrowserRuntime["fingerprintSupport"];
      managed?: boolean;
      sha256?: string;
    }>;
    return raw.map((r) => ({
      id: r.id,
      name: r.name,
      version: r.version,
      channel: r.channel as BrowserRuntime["channel"],
      trusted: r.trusted,
      active: r.active,
      fingerprintSupport: r.fingerprintSupport ?? "unverified",
      managed: r.managed ?? false,
      sha256: r.sha256,
    }));
  }

  async probeRuntimeFingerprint(runtimeId: string): Promise<RuntimeFingerprintProbeResult> {
    return this.call<RuntimeFingerprintProbeResult>(handlers.probeRuntimeFingerprint, {
      request: { runtimeId },
    });
  }

  async inspectLegacyMigration(profileId: string, sourceProfileId: string): Promise<LegacyMigrationInspection> {
    return this.call<LegacyMigrationInspection>(handlers.legacyMigrationInspect, {
      request: { profileId, sourceProfileId },
    });
  }

  async executeLegacyMigration(profileId: string, sourceProfileId: string): Promise<LegacyMigrationRecord> {
    return this.call<LegacyMigrationRecord>(handlers.legacyMigrationExecute, {
      request: { profileId, sourceProfileId },
    });
  }

  async rollbackLegacyMigration(migrationId: number): Promise<LegacyMigrationRecord> {
    return this.call<LegacyMigrationRecord>(handlers.legacyMigrationRollback, {
      request: { migrationId },
    });
  }

  async listLegacyMigrations(limit = 50): Promise<LegacyMigrationRecord[]> {
    return this.call<LegacyMigrationRecord[]>(handlers.legacyMigrationList, {
      request: { limit },
    });
  }

  async listProfiles(includeArchived = false): Promise<Profile[]> {
    // Observe both failures immediately, even while the profile pages are loading.
    const [profiles] = await Promise.all([
      this.listProfileDtos(includeArchived),
      this.refreshSessions(),
    ]);
    profiles.forEach((profile) => this.cacheProfile(profile));
    return profiles.map((profile) => this.toProfile(profile));
  }

  private async listProfileDtos(includeArchived: boolean): Promise<ProfileDto[]> {
    const profiles: ProfileDto[] = [];
    for (let page = 1; ; page += 1) {
      const batch = await this.call<ProfileDto[]>(handlers.profileList, {
        request: { page, pageSize: 200, search: null, includeArchived },
      });
      profiles.push(...batch);
      if (batch.length < 200) break;
    }
    return profiles;
  }

  async getProfile(id: string): Promise<Profile> {
    const [profile] = await Promise.all([
      this.call<ProfileDto>(handlers.profileGet, { request: { profileId: id } }),
      this.refreshSessions(),
    ]);
    this.cacheProfile(profile);
    return this.toProfile(profile);
  }

  async createProfile(input: ProfileInput): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileCreate, {
      request: { profile: draftFromInput(input) },
    });
    this.cacheProfile(profile);
    return this.toProfile(profile);
  }

  async updateProfile(
    id: string,
    revision: number,
    changes: Partial<ProfileInput>,
  ): Promise<Profile> {
    // Reuse cached fields only when they match the edit's expected revision.
    const current = await this.getDto(id, revision);
    const profile = await this.call<ProfileDto>(handlers.profileUpdate, {
      request: {
        profileId: id,
        profile: mergeDraft(current, changes),
        expectedRevision: revision,
      },
    });
    this.cacheProfile(profile);
    return this.toProfile(profile);
  }

  async cloneProfile(id: string): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileClone, {
      request: { profileId: id },
    });
    this.cacheProfile(profile);
    return this.toProfile(profile);
  }

  async archiveProfile(id: string, revision: number): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileArchive, {
      request: { profileId: id, expectedRevision: revision },
    });
    this.cacheProfile(profile);
    return this.toProfile(profile);
  }

  async restoreProfile(id: string, revision: number): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileRestore, {
      request: { profileId: id, expectedRevision: revision },
    });
    this.cacheProfile(profile);
    return this.toProfile(profile);
  }

  async launchSession(profileId: string): Promise<Profile> {
    const session = await this.call<SessionDto>(handlers.sessionLaunch, {
      request: { profileId },
    });
    this.upsertSession(session);
    return this.getProfileView(profileId);
  }

  async stopSession(profileId: string): Promise<Profile> {
    await this.refreshSessions();
    const session = newestActiveSession(this.sessions, profileId);
    if (!session) throw new ApiError("NOT_FOUND", "找不到该环境的活动会话");
    const stopped = await this.call<SessionDto>(
      session.state === "queued" ? handlers.sessionCancelQueued : handlers.sessionStop,
      { request: { sessionId: session.id } },
    );
    this.upsertSession(stopped);
    return this.getProfileView(profileId);
  }

  async getNetworkProbeService(): Promise<NetworkProbeService | undefined> {
    return (await this.call<NetworkProbeService | null>(handlers.networkProbeService)) ?? undefined;
  }
  async probeSessionNetwork(profileId: string): Promise<NetworkProbeObservation> {
    await this.refreshSessions();
    const session = newestActiveSession(this.sessions, profileId);
    if (!session || session.state !== "running") throw new ApiError("INVALID_PROFILE_STATE", "请先启动要检测的环境");
    const report = await this.call<NetworkProbeObservation>(handlers.sessionProbeNetwork, { request: { sessionId: session.id, acknowledgeExternal: true } });
    await this.ensureSameRunningSession(profileId, session.id);
    return report;
  }

  async observeSession(profileId: string): Promise<SessionObservation> {
    await this.refreshSessions();
    const session = newestActiveSession(this.sessions, profileId);
    if (!session || session.state !== "running") throw new ApiError("INVALID_PROFILE_STATE", "请先启动要观测的环境");
    const report = await this.call<SessionObservation>(handlers.sessionObserve, { request: { sessionId: session.id } });
    if (report.sessionId !== session.id) throw new ApiError("INVALID_PROFILE_STATE", "观测结果的会话已失效，请重新读取");
    await this.ensureSameRunningSession(profileId, session.id);
    return report;
  }

  private async ensureSameRunningSession(profileId: string, sessionId: string): Promise<void> {
    await this.refreshSessions();
    const current = newestActiveSession(this.sessions, profileId);
    if (!current || current.state !== "running" || current.id !== sessionId) {
      throw new ApiError("INVALID_PROFILE_STATE", "检测期间会话已停止或更换，请重新检查");
    }
  }

  async cancelQueuedSession(sessionId: string): Promise<SessionSummary> {
    const session = await this.call<SessionDto>(handlers.sessionCancelQueued, {
      request: { sessionId },
    });
    this.upsertSession(session);
    return toSessionSummary(session);
  }

  async listProxies(): Promise<ProxySummary[]> {
    const proxies = await this.call<ProxyDto[]>(handlers.proxyList);
    return proxies.map((proxy) => this.toProxy(proxy));
  }
  async saveProxy(input: ProxyInput): Promise<ProxySummary> {
    const proxy = await this.call<ProxyDto>(handlers.proxySave, {
      request: {
        proxy: {
          name: input.name,
          scheme: input.kind,
          host: input.host,
          port: input.port,
          username: input.username,
          dnsMode: "proxy",
          bypassHosts: [],
          connectTimeoutMs: 10_000,
        },
        credential: input.credential,
      },
    });
    return this.toProxy(proxy);
  }
  async testProxy(id: string): Promise<ProxySummary> {
    let result: ProxyConnectivityCheckDto;
    try {
      result = await this.call<ProxyConnectivityCheckDto>(handlers.proxyTest, {
        request: { proxyId: id },
      });
    } catch (error) {
      this.proxyHealth.set(id, {
        health: "unhealthy",
        lastCheckedAt: new Date().toISOString(),
      });
      throw error;
    }
    this.proxyHealth.set(id, {
      health: result.reachable ? "healthy" : "unhealthy",
      lastCheckedAt: new Date().toISOString(),
      lastExitIp: result.exitIp,
      exitIpChecked: result.exitIpChecked,
    });
    const proxy = (await this.call<ProxyDto[]>(handlers.proxyList)).find((item) => item.id === id);
    if (!proxy) throw new ApiError("NOT_FOUND", "代理不存在");
    return this.toProxy(proxy);
  }
  async removeProxy(id: string): Promise<void> {
    await this.call<void>(handlers.proxyRemove, { request: { proxyId: id } });
    this.proxyHealth.delete(id);
  }
  async listDiagnostics(profileId?: string): Promise<DiagnosticReport[]> {
    const reports = await this.call<DiagnosticReportDto[]>(handlers.diagnosticList, { request: { profileId: profileId ?? null } });
    return reports.map((report) => this.toDiagnostic(report));
  }
  async runDiagnostic(profileId: string, external = false): Promise<DiagnosticReport> {
    const report = await this.call<DiagnosticReportDto>(handlers.diagnosticRun, { request: { profileId, external } });
    return this.toDiagnostic(report);
  }
  async listBackups(): Promise<BackupJob[]> {
    const jobs = await this.call<BackupJobDto[]>(handlers.backupList);
    return jobs.map((job) => ({ ...job }));
  }
  async exportBackup(
    profileId: string,
    destinationName: string,
    includeProxySecret: boolean,
    password: string,
  ): Promise<BackupJob> {
    const job = await this.call<BackupJobDto>(handlers.backupExport, {
      request: { profileId, destinationName, password, includeProxyCredential: includeProxySecret },
    });
    return { ...job };
  }
  async importBackup(sourceName: string, password: string): Promise<Profile> {
    const result = await this.call<BackupImportResultDto>(handlers.backupImport, {
      request: { sourceName, password },
    });
    this.cacheProfile(result.profile);
    return this.toProfile(result.profile);
  }
  async getSettings(): Promise<Settings> {
    return this.call<Settings>(handlers.settingsGet);
  }
  async captureRuntime(runtimeId: string): Promise<BrowserRuntime> {
    return this.call<BrowserRuntime>(handlers.runtimeCapture, { request: { runtimeId } });
  }
  async activateRuntime(runtimeId: string): Promise<void> {
    await this.call<void>(handlers.runtimeActivate, { request: { runtimeId } });
  }
  async getSupportReport(): Promise<Record<string, unknown>> {
    return this.call<Record<string, unknown>>(handlers.supportReport);
  }
  async updateSettings(changes: Partial<Settings>): Promise<Settings> {
    return this.call<Settings>(handlers.settingsUpdate, { request: { changes } });
  }

  private cacheProfile(profile: ProfileDto): void {
    const cached = this.dtoCache.get(profile.id);
    // Reads and mutations can finish out of order. An older response must not
    // replace fields that a subsequent partial update will merge into its draft.
    if (!cached || profile.revision >= cached.revision) {
      this.dtoCache.set(profile.id, profile);
    }
  }

  private async getDto(id: string, revision?: number): Promise<ProfileDto> {
    const cached = this.dtoCache.get(id);
    if (cached && (revision === undefined || cached.revision === revision)) return cached;
    const profile = await this.call<ProfileDto>(handlers.profileGet, {
      request: { profileId: id },
    });
    this.cacheProfile(profile);
    return profile;
  }

  private async getProfileView(id: string): Promise<Profile> {
    const dto = await this.getDto(id);
    return this.toProfile(dto);
  }

  private async refreshSessions(): Promise<void> {
    const sequence = ++this.sessionSequence;
    const sessions = await this.call<SessionDto[]>(handlers.sessionList);
    // A late read must not replace a newer read or a completed session action.
    if (sequence > this.appliedSessionSequence) {
      this.sessions = sessions;
      this.appliedSessionSequence = sequence;
    }
  }

  private upsertSession(session: SessionDto): void {
    this.appliedSessionSequence = ++this.sessionSequence;
    this.sessions = [session, ...this.sessions.filter((item) => item.id !== session.id)];
  }

  private toProfile(dto: ProfileDto): Profile {
    const session = newestActiveSession(this.sessions, dto.id) ?? newestSession(this.sessions, dto.id);
    const createdAt = this.firstSeen.get(dto.id) ?? new Date().toISOString();
    this.firstSeen.set(dto.id, createdAt);
    return {
      id: dto.id,
      name: dto.name,
      tags: [...dto.tags],
      presetId: dto.presetId,
      proxyId: dto.proxyId,
      runtimeId: runtimeId(dto.browser),
      status: dto.archived ? "archived" : (session?.state ?? "stopped"),
      revision: dto.revision,
      createdAt,
      lastUsedAt: session ? new Date(session.updatedAtMs).toISOString() : undefined,
      diagnostic: { overall: "not-run", issueCount: 0 },
      persona: personaFromDto(dto.persona),
      acceptLanguages: [...dto.locale.acceptLanguages],
      timezone: dto.locale.timezone,
      windowWidth: dto.window.width,
      windowHeight: dto.window.height,
    };
  }

  private toProxy(dto: ProxyDto): ProxySummary {
    const check = this.proxyHealth.get(dto.id);
    return {
      id: dto.id,
      name: dto.name,
      kind: dto.scheme,
      host: dto.host,
      port: dto.port,
      authenticated: dto.hasSecret,
      health: check?.health ?? "unknown",
      lastCheckedAt: check?.lastCheckedAt,
      lastExitIp: check?.lastExitIp,
      exitIpChecked: check?.exitIpChecked,
    };
  }

  private toDiagnostic(dto: DiagnosticReportDto): DiagnosticReport {
    return {
      profileId: dto.profileId,
      checkedAt: new Date(dto.generatedAtMs).toISOString(),
      overall: dto.overall === "healthy" ? "pass" : dto.overall === "failed" ? "fail" : "warning",
      items: dto.checks.map((check) => ({
        key: check.key,
        label: check.key,
        actual: check.observed ?? "未检查",
        expected: check.expected,
        scope: check.scope === "host_shared" ? "host-shared" : check.scope,
        status: check.status === "warn" ? "warning" : check.status === "not_checked" ? "not-checked" : check.status,
        explanation: check.explanation,
      })),
      externalEndpoints: dto.externalEndpoints,
    };
  }

  private async call<T>(
    handler: (typeof handlers)[keyof typeof handlers],
    args?: Record<string, unknown>,
  ): Promise<T> {
    try {
      return (await this.bridge.invoke(handler, args)) as T;
    } catch (error) {
      throw normalizeError(error);
    }
  }
}

function personaFromDto(dto: ProfileDto["persona"]): PersonaConfig {
  if ("system_native" in dto) return { kind: "system_native" };
  const value = dto.seedable_noise;
  return {
    kind: "seedable_noise",
    seedable: {
      seed: value.seed,
      platform: value.platform,
      hardwareConcurrency: value.hardwareConcurrency,
      deviceMemory: ({ quarter: "0.25", half: "0.5", one: "1", two: "2", four: "4", eight: "8" } as const)[value.deviceMemory],
      touchPoints: value.touchPoints,
      screenWidth: value.screen.width,
      screenHeight: value.screen.height,
      colorDepth: value.screen.colorDepth,
      deviceScaleFactor: value.screen.deviceScaleFactorHundredths / 100,
      webglVendor: value.webglVendor,
      webglRenderer: value.webglRenderer,
      spoofCanvas: value.spoofCanvas,
      spoofAudio: value.spoofAudio,
    },
  };
}

function draftFromInput(input: ProfileInput): ProfileDraftDto {
  return {
    name: input.name,
    tags: [...input.tags],
    presetId: input.presetId,
    presetVersion: 1,
    proxyId: input.proxyId || undefined,
    browser: { mode: "pinned", value: { buildId: input.runtimeId } },
    persona: buildPersonaDto(input.persona) as ProfileDraftDto extends never ? never : ProfileDraftDto["persona"],
    locale: { acceptLanguages: [...(input.acceptLanguages ?? ["zh-CN", "zh"])], timezone: input.persona?.kind === "seedable_noise" ? input.timezone : undefined },
    window: { width: input.windowWidth ?? 1280, height: input.windowHeight ?? 800, startMaximized: false },
    startupUrls: [],
  };
}

/** Convert a UI `PersonaConfig` into the Rust DTO shape.
 * Defaults to SystemNative when no persona is supplied.
 */
function buildPersonaDto(config: PersonaConfig | undefined): ProfileDraftDto["persona"] {
  const input = config;
  if (input?.kind === "seedable_noise") {
    const cfg = input.seedable;
    if (!cfg) return systemNativePersona();
    return {
      seedable_noise: {
        seed: cfg.seed,
        platform: cfg.platform === "mac_os" ? "mac_os" : cfg.platform,
        hardwareConcurrency: cfg.hardwareConcurrency,
        deviceMemory: deviceMemoryDto(cfg.deviceMemory),
        touchPoints: cfg.touchPoints,
        screen: {
          width: cfg.screenWidth,
          height: cfg.screenHeight,
          colorDepth: cfg.colorDepth,
          deviceScaleFactorHundredths: Math.round(cfg.deviceScaleFactor * 100),
        },
        webglVendor: cfg.webglVendor,
        webglRenderer: cfg.webglRenderer,
        spoofCanvas: cfg.spoofCanvas,
        spoofAudio: cfg.spoofAudio,
      },
    };
  }
  return systemNativePersona();
}

function deviceMemoryDto(value: string): "quarter" | "half" | "one" | "two" | "four" | "eight" {
  switch (value) {
    case "0.25": return "quarter";
    case "0.5": return "half";
    case "1": return "one";
    case "2": return "two";
    case "4": return "four";
    case "8": return "eight";
    default: return "eight";
  }
}

function systemNativePersona(): ProfileDraftDto["persona"] {
  return {
    system_native: {
      preserveSystemTimezone: true,
      preserveSystemFonts: true,
      preserveSystemGpu: true,
      preserveSystemAudio: true,
    },
  };
}

function mergeDraft(dto: ProfileDto, changes: Partial<ProfileInput>): ProfileDraftDto {
  return {
    name: changes.name ?? dto.name,
    tags: changes.tags ? [...changes.tags] : [...dto.tags],
    presetId: changes.presetId ?? dto.presetId,
    presetVersion: changes.presetId && changes.presetId !== dto.presetId ? 1 : dto.presetVersion,
    proxyId: Object.prototype.hasOwnProperty.call(changes, "proxyId")
      ? changes.proxyId || undefined
      : dto.proxyId,
    browser: changes.runtimeId
      ? { mode: "pinned", value: { buildId: changes.runtimeId } }
      : dto.browser,
    persona: Object.prototype.hasOwnProperty.call(changes, "persona")
      ? buildPersonaDto(changes.persona)
      : dto.persona,
    locale: {
      ...dto.locale,
      acceptLanguages: [...(changes.acceptLanguages ?? dto.locale.acceptLanguages)],
      timezone: changes.persona?.kind === "system_native" ? undefined
        : Object.prototype.hasOwnProperty.call(changes, "timezone") ? changes.timezone : dto.locale.timezone,
    },
    window: {
      ...dto.window,
      width: changes.windowWidth ?? dto.window.width,
      height: changes.windowHeight ?? dto.window.height,
    },
    startupUrls: [...dto.startupUrls],
  };
}

function runtimeId(browser: BrowserSelectorDto): string {
  return browser.mode === "pinned" ? browser.value.buildId : `channel:${browser.value.channel}`;
}

function newestActiveSession(sessions: SessionDto[], profileId: string): SessionDto | undefined {
  return newestSession(sessions, profileId, true);
}

function newestSession(sessions: SessionDto[], profileId: string, activeOnly = false): SessionDto | undefined {
  let newest: SessionDto | undefined;
  for (const session of sessions) {
    if (session.profileId !== profileId) continue;
    if (activeOnly && (session.state === "stopped" || session.state === "failed")) continue;
    if (!newest || session.createdAtMs > newest.createdAtMs ||
      (session.createdAtMs === newest.createdAtMs && session.updatedAtMs > newest.updatedAtMs)) {
      newest = session;
    }
  }
  return newest;
}

function toSessionSummary(session: SessionDto): SessionSummary {
  return {
    id: session.id,
    profileId: session.profileId,
    state: session.state,
    createdAtMs: session.createdAtMs,
    updatedAtMs: session.updatedAtMs,
    failureCode: session.failureCode,
  };
}

function normalizeError(error: unknown): ApiError {
  if (error instanceof ApiError) return error;
  if (typeof error === "string") {
    try {
      return normalizeError(JSON.parse(error) as unknown);
    } catch {
      return new ApiError("INTERNAL", error);
    }
  }
  if (typeof error === "object" && error !== null) {
    const dto = error as ErrorDto;
    if (typeof dto.code === "string" && typeof dto.message === "string") {
      return new ApiError(dto.code, dto.message, dto.violations);
    }
  }
  return new ApiError(
    "INTERNAL",
    error instanceof Error ? error.message : "桌面命令执行失败",
  );
}
