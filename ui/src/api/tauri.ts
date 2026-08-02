import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import {
  ApiError,
  EMPTY_SETTINGS,
  type ApiClient,
  type BackupJob,
  type BrowserRuntime,
  type DiagnosticReport,
  type LegacyMigrationInspection,
  type LegacyMigrationRecord,
  type MoreLoginImportResult,
  type MoreLoginPreview,
  type Profile,
  type ProfileInput,
  type ProfileStatus,
  type ProxyInput,
  type ProxySummary,
  type SessionSummary,
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
  moreLoginPreview: "morelogin_preview",
  moreLoginImport: "morelogin_import",
  legacyMigrationInspect: "legacy_migration_inspect",
  legacyMigrationExecute: "legacy_migration_execute",
  legacyMigrationRollback: "legacy_migration_rollback",
  legacyMigrationList: "legacy_migration_list",
});

const prototypeRuntime: BrowserRuntime = {
  id: "google-chrome-stable-150",
  name: "Google Chrome",
  version: "150.0.7871.188",
  channel: "stable",
  trusted: true,
  active: true,
};

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function createTauriBridge(): TypedCommandBridge {
  return {
    invoke(handler, args) {
      return tauriInvoke(handler, args);
    },
  };
}

/** Maps the versioned Rust DTOs to stable UI view models. */
export class TauriApiClient implements ApiClient {
  private readonly dtoCache = new Map<string, ProfileDto>();
  private readonly firstSeen = new Map<string, string>();
  private sessions: SessionDto[] = [];

  constructor(private readonly bridge: TypedCommandBridge) {}

  async previewMoreLogin(sourcePath: string): Promise<MoreLoginPreview> {
    return this.call<MoreLoginPreview>(handlers.moreLoginPreview, { request: { sourcePath } });
  }

  async importMoreLogin(sourcePath: string, rowNumbers: number[] = []): Promise<MoreLoginImportResult> {
    return this.call<MoreLoginImportResult>(handlers.moreLoginImport, {
      request: { sourcePath, rowNumbers },
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
    const sessionsPromise = this.call<SessionDto[]>(handlers.sessionList);
    const profiles: ProfileDto[] = [];
    for (let page = 1; ; page += 1) {
      const batch = await this.call<ProfileDto[]>(handlers.profileList, {
        request: { page, pageSize: 200, search: null, includeArchived },
      });
      profiles.push(...batch);
      if (batch.length < 200) break;
    }
    const sessions = await sessionsPromise;
    this.sessions = sessions;
    profiles.forEach((profile) => this.dtoCache.set(profile.id, profile));
    return profiles.map((profile) => this.toProfile(profile));
  }

  async getProfile(id: string): Promise<Profile> {
    const [profile, sessions] = await Promise.all([
      this.call<ProfileDto>(handlers.profileGet, { request: { profileId: id } }),
      this.call<SessionDto[]>(handlers.sessionList),
    ]);
    this.sessions = sessions;
    this.dtoCache.set(profile.id, profile);
    return this.toProfile(profile);
  }

  async createProfile(input: ProfileInput): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileCreate, {
      request: { profile: draftFromInput(input) },
    });
    this.dtoCache.set(profile.id, profile);
    return this.toProfile(profile);
  }

  async updateProfile(
    id: string,
    revision: number,
    changes: Partial<ProfileInput>,
  ): Promise<Profile> {
    const current = await this.getDto(id);
    const profile = await this.call<ProfileDto>(handlers.profileUpdate, {
      request: {
        profileId: id,
        profile: mergeDraft(current, changes),
        expectedRevision: revision,
      },
    });
    this.dtoCache.set(profile.id, profile);
    return this.toProfile(profile);
  }

  async cloneProfile(id: string): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileClone, {
      request: { profileId: id },
    });
    this.dtoCache.set(profile.id, profile);
    return this.toProfile(profile);
  }

  async archiveProfile(id: string, revision: number): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileArchive, {
      request: { profileId: id, expectedRevision: revision },
    });
    this.dtoCache.set(profile.id, profile);
    return this.toProfile(profile);
  }

  async restoreProfile(id: string, revision: number): Promise<Profile> {
    const profile = await this.call<ProfileDto>(handlers.profileRestore, {
      request: { profileId: id, expectedRevision: revision },
    });
    this.dtoCache.set(profile.id, profile);
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

  async cancelQueuedSession(sessionId: string): Promise<SessionSummary> {
    const session = await this.call<SessionDto>(handlers.sessionCancelQueued, {
      request: { sessionId },
    });
    this.upsertSession(session);
    return toSessionSummary(session);
  }

  async listProxies(): Promise<ProxySummary[]> {
    return [];
  }
  async saveProxy(_input: ProxyInput): Promise<ProxySummary> {
    return unsupported("代理保存");
  }
  async testProxy(_id: string): Promise<ProxySummary> {
    return unsupported("代理检测");
  }
  async listDiagnostics(_profileId?: string): Promise<DiagnosticReport[]> {
    return [];
  }
  async runDiagnostic(_profileId: string): Promise<DiagnosticReport> {
    return unsupported("环境诊断");
  }
  async listBackups(): Promise<BackupJob[]> {
    return [];
  }
  async exportBackup(
    _profileId: string,
    _destinationName: string,
    _includeProxySecret: boolean,
  ): Promise<BackupJob> {
    return unsupported("加密备份");
  }
  async listRuntimes(): Promise<BrowserRuntime[]> {
    return [{ ...prototypeRuntime }];
  }
  async getSettings(): Promise<Settings> {
    return { ...EMPTY_SETTINGS };
  }
  async updateSettings(_changes: Partial<Settings>): Promise<Settings> {
    return unsupported("设置持久化");
  }

  private async getDto(id: string): Promise<ProfileDto> {
    const cached = this.dtoCache.get(id);
    if (cached) return cached;
    const profile = await this.call<ProfileDto>(handlers.profileGet, {
      request: { profileId: id },
    });
    this.dtoCache.set(id, profile);
    return profile;
  }

  private async getProfileView(id: string): Promise<Profile> {
    const dto = await this.getDto(id);
    return this.toProfile(dto);
  }

  private async refreshSessions(): Promise<void> {
    this.sessions = await this.call<SessionDto[]>(handlers.sessionList);
  }

  private upsertSession(session: SessionDto): void {
    this.sessions = [session, ...this.sessions.filter((item) => item.id !== session.id)];
  }

  private toProfile(dto: ProfileDto): Profile {
    const session = newestActiveSession(this.sessions, dto.id);
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

function draftFromInput(input: ProfileInput): ProfileDraftDto {
  return {
    name: input.name,
    tags: [...input.tags],
    presetId: input.presetId,
    presetVersion: 1,
    proxyId: input.proxyId,
    browser: { mode: "pinned", value: { buildId: input.runtimeId } },
    persona: {
      system_native: {
        preserveSystemTimezone: true,
        preserveSystemFonts: true,
        preserveSystemGpu: true,
        preserveSystemAudio: true,
      },
    },
    locale: { acceptLanguages: ["zh-CN", "zh"] },
    window: { width: 1280, height: 800, startMaximized: false },
    startupUrls: [],
  };
}

function mergeDraft(dto: ProfileDto, changes: Partial<ProfileInput>): ProfileDraftDto {
  return {
    name: changes.name ?? dto.name,
    tags: changes.tags ? [...changes.tags] : [...dto.tags],
    presetId: changes.presetId ?? dto.presetId,
    presetVersion: changes.presetId && changes.presetId !== dto.presetId ? 1 : dto.presetVersion,
    proxyId: Object.prototype.hasOwnProperty.call(changes, "proxyId")
      ? changes.proxyId
      : dto.proxyId,
    browser: changes.runtimeId
      ? { mode: "pinned", value: { buildId: changes.runtimeId } }
      : dto.browser,
    persona: dto.persona,
    locale: dto.locale,
    window: dto.window,
    startupUrls: [...dto.startupUrls],
  };
}

function runtimeId(browser: BrowserSelectorDto): string {
  return browser.mode === "pinned" ? browser.value.buildId : `channel:${browser.value.channel}`;
}

function newestActiveSession(sessions: SessionDto[], profileId: string): SessionDto | undefined {
  return sessions
    .filter(
      (session) =>
        session.profileId === profileId &&
        !["stopped", "failed"].includes(session.state),
    )
    .sort((left, right) => right.createdAtMs - left.createdAtMs)[0];
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

function unsupported<T>(capability: string): Promise<T> {
  return Promise.reject(
    new ApiError("CAPABILITY_UNSUPPORTED", `${capability}尚未接入本地适配器`),
  );
}
