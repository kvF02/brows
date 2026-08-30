import {
  ApiError,
  DEFAULT_PRESETS,
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
  type ProxyInput,
  type ProxySummary,
  type Settings,
  type SessionSummary,
} from "../types";

const now = () => new Date().toISOString();

function newId(prefix: string): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return `${prefix}-${crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

const runtimes: BrowserRuntime[] = [
  {
    id: "chrome-testing-stable",
    name: "Chrome for Testing",
    version: "127.0.6533.89",
    channel: "stable",
    trusted: true,
    active: true,
    sha256: "演示数据：接入运行时管理器后显示",
  },
];

const initialProxies: ProxySummary[] = [
  {
    id: "proxy-local",
    name: "本机直连（仅测试）",
    kind: "http",
    host: "系统网络",
    port: 0,
    authenticated: false,
    health: "unknown",
  },
];

function makeProfile(
  id: string,
  name: string,
  presetId: string,
  proxyId?: string,
): Profile {
  return {
    id,
    name,
    tags: presetId === "system-native" ? ["默认", "系统原生"] : ["研究"],
    presetId,
    proxyId,
    runtimeId: runtimes[0].id,
    status: "stopped",
    revision: 1,
    createdAt: now(),
    diagnostic: { overall: "not-run", issueCount: 0 },
  };
}

/** In-memory preview adapter; the desktop build injects TauriApiClient. */
export class MockApiClient implements ApiClient {
  private profiles: Profile[] = [
    makeProfile("profile-demo-1", "研究环境 · 东京", "system-native", "proxy-local"),
    makeProfile("profile-demo-2", "QA 环境 · 原生", "system-native"),
    makeProfile("profile-demo-3", "社媒测试 · 美国", "system-native"),
  ];
  private proxies = [...initialProxies];
  private diagnostics: DiagnosticReport[] = [];
  private backups: BackupJob[] = [];
  private settings: Settings = { ...EMPTY_SETTINGS };
  private sessions = new Map<string, SessionSummary>();

  async previewMoreLogin(_sourcePath: string): Promise<MoreLoginPreview> {
    throw new ApiError("CAPABILITY_UNSUPPORTED", "MoreLogin 导入仅在桌面版中可用");
  }

  async fingerprintPreview(input: {
    persona: { kind?: string; seedable?: { seed?: string } };
    acceptLanguages: string[];
    windowWidth: number;
    windowHeight: number;
  }): Promise<{
    effectiveUserAgent: string;
    effectivePlatform: string;
    effectiveLanguages: string[];
    effectiveWindowWidth: number;
    effectiveWindowHeight: number;
    effectiveWebglVendor: string;
    effectiveWebglRenderer: string;
    hardwareConcurrency: number;
    deviceMemoryGb: string;
    declaredOnlyFields: string[];
    hostSharedFields: string[];
  }> {
    // Browser-only preview: mock returns a deterministic but clearly fake result.
    const seed = input.persona.seedable?.seed ?? "mock";
    return {
      effectiveUserAgent: `Mozilla/5.0 (mock) Chrome/150.0.0.0 Safari/537.36 (seed=${seed})`,
      effectivePlatform: "mock",
      effectiveLanguages: input.acceptLanguages,
      effectiveWindowWidth: input.windowWidth,
      effectiveWindowHeight: input.windowHeight,
      effectiveWebglVendor: "Mock GPU",
      effectiveWebglRenderer: "Mock Renderer",
      hardwareConcurrency: 8,
      deviceMemoryGb: "8",
      declaredOnlyFields: [],
      hostSharedFields: [],
    };
  }

  async importMoreLogin(_sourcePath: string, _rowNumbers: number[] = []): Promise<MoreLoginImportResult> {
    throw new ApiError("CAPABILITY_UNSUPPORTED", "MoreLogin 导入仅在桌面版中可用");
  }

  async inspectLegacyMigration(_profileId: string, _sourceProfileId: string): Promise<LegacyMigrationInspection> {
    throw new ApiError("CAPABILITY_UNSUPPORTED", "完整环境迁移仅在桌面版中可用");
  }

  async executeLegacyMigration(_profileId: string, _sourceProfileId: string): Promise<LegacyMigrationRecord> {
    throw new ApiError("CAPABILITY_UNSUPPORTED", "完整环境迁移仅在桌面版中可用");
  }

  async rollbackLegacyMigration(_migrationId: number): Promise<LegacyMigrationRecord> {
    throw new ApiError("CAPABILITY_UNSUPPORTED", "完整环境迁移仅在桌面版中可用");
  }

  async listLegacyMigrations(_limit = 50): Promise<LegacyMigrationRecord[]> {
    return [];
  }

  async listProfiles(includeArchived = false): Promise<Profile[]> {
    return this.profiles
      .filter((profile) => includeArchived || profile.status !== "archived")
      .map((profile) => ({ ...profile, tags: [...profile.tags] }));
  }

  async getProfile(id: string): Promise<Profile> {
    const profile = this.findProfile(id);
    return { ...profile, tags: [...profile.tags] };
  }

  async createProfile(input: ProfileInput): Promise<Profile> {
    const name = input.name.trim();
    if (!name) throw new ApiError("VALIDATION_FAILED", "环境名称不能为空");
    if (this.profiles.some((profile) => profile.name.toLowerCase() === name.toLowerCase())) {
      throw new ApiError("VALIDATION_FAILED", "环境名称已经存在");
    }
    const profile: Profile = {
      id: newId("profile"),
      name,
      tags: [...input.tags],
      presetId: input.presetId,
      proxyId: input.proxyId,
      runtimeId: input.runtimeId,
      status: "stopped",
      revision: 1,
      createdAt: now(),
      diagnostic: { overall: "not-run", issueCount: 0 },
    };
    this.profiles = [profile, ...this.profiles];
    return { ...profile, tags: [...profile.tags] };
  }

  async updateProfile(
    id: string,
    revision: number,
    changes: Partial<ProfileInput>,
  ): Promise<Profile> {
    const profile = this.findProfile(id);
    if (profile.revision !== revision) {
      throw new ApiError("REVISION_CONFLICT", "环境已被其他操作更新，请刷新后重试");
    }
    if (profile.status === "running" || profile.status === "starting") {
      throw new ApiError("INVALID_PROFILE_STATE", "运行中的环境不能修改配置");
    }
    Object.assign(profile, changes, { revision: profile.revision + 1 });
    return { ...profile, tags: [...profile.tags] };
  }

  async cloneProfile(id: string): Promise<Profile> {
    const source = this.findProfile(id);
    return this.createProfile({
      name: `${source.name} · 副本`,
      tags: [...source.tags, "副本"],
      presetId: source.presetId,
      proxyId: source.proxyId,
      runtimeId: source.runtimeId,
    });
  }

  async archiveProfile(id: string, revision: number): Promise<Profile> {
    const profile = this.findProfile(id);
    if (profile.revision !== revision) {
      throw new ApiError("REVISION_CONFLICT", "环境已被其他操作更新，请刷新后重试");
    }
    if (profile.status !== "stopped" && profile.status !== "failed") {
      throw new ApiError("INVALID_PROFILE_STATE", "请先停止环境再归档");
    }
    profile.status = "archived";
    profile.revision += 1;
    return { ...profile, tags: [...profile.tags] };
  }

  async restoreProfile(id: string, revision: number): Promise<Profile> {
    const profile = this.findProfile(id);
    if (profile.revision !== revision) {
      throw new ApiError("REVISION_CONFLICT", "环境已被其他操作更新，请刷新后重试");
    }
    if (profile.status !== "archived") {
      throw new ApiError("INVALID_PROFILE_STATE", "只有已归档环境可以恢复");
    }
    profile.status = "stopped";
    profile.revision += 1;
    return { ...profile, tags: [...profile.tags] };
  }

  async launchSession(profileId: string): Promise<Profile> {
    const profile = this.findProfile(profileId);
    if (profile.status === "archived") {
      throw new ApiError("INVALID_PROFILE_STATE", "归档环境不能启动");
    }
    if (profile.status === "running" || profile.status === "starting") {
      return { ...profile, tags: [...profile.tags] };
    }
    const running = this.profiles.filter((item) => item.status === "running").length;
    if (running >= this.settings.maxConcurrentSessions) {
      profile.status = "queued";
    } else {
      profile.status = "running";
      profile.lastUsedAt = now();
    }
    const timestamp = Date.now();
    const session: SessionSummary = {
      id: newId("session"),
      profileId,
      state: profile.status === "queued" ? "queued" : "running",
      createdAtMs: timestamp,
      updatedAtMs: timestamp,
    };
    this.sessions.set(session.id, session);
    profile.revision += 1;
    return { ...profile, tags: [...profile.tags] };
  }

  async stopSession(profileId: string): Promise<Profile> {
    const profile = this.findProfile(profileId);
    const session = [...this.sessions.values()].find(
      (item) => item.profileId === profileId && item.state !== "stopped",
    );
    if (session) {
      session.state = "stopped";
      session.updatedAtMs = Date.now();
    }
    profile.status = "stopped";
    profile.revision += 1;
    return { ...profile, tags: [...profile.tags] };
  }

  async cancelQueuedSession(sessionId: string): Promise<SessionSummary> {
    const session = this.sessions.get(sessionId);
    if (!session) throw new ApiError("NOT_FOUND", "找不到排队会话");
    if (session.state !== "queued") {
      throw new ApiError("INVALID_PROFILE_STATE", "只有排队会话可以取消");
    }
    session.state = "stopped";
    session.updatedAtMs = Date.now();
    const profile = this.findProfile(session.profileId);
    profile.status = "stopped";
    return { ...session };
  }

  async listProxies(): Promise<ProxySummary[]> {
    return this.proxies.map((proxy) => ({ ...proxy }));
  }

  async saveProxy(input: ProxyInput): Promise<ProxySummary> {
    if (!input.name.trim() || !input.host.trim() || input.port < 1 || input.port > 65535) {
      throw new ApiError("VALIDATION_FAILED", "请填写有效的代理名称、主机和端口");
    }
    const proxy: ProxySummary = {
      id: newId("proxy"),
      name: input.name.trim(),
      kind: input.kind,
      host: input.host.trim(),
      port: input.port,
      authenticated: Boolean(input.username || input.credential),
      health: "unknown",
    };
    this.proxies = [proxy, ...this.proxies];
    return { ...proxy };
  }

  async testProxy(id: string): Promise<ProxySummary> {
    const proxy = this.proxies.find((item) => item.id === id);
    if (!proxy) throw new ApiError("NOT_FOUND", "找不到代理");
    proxy.health = "checking";
    await new Promise((resolve) => setTimeout(resolve, 450));
    proxy.health = proxy.id === "proxy-local" ? "unknown" : "healthy";
    proxy.lastCheckedAt = now();
    if (proxy.health === "healthy") proxy.lastExitIp = "检测后由运行时填充";
    return { ...proxy };
  }

  async removeProxy(id: string): Promise<void> {
    if (this.profiles.some((profile) => profile.proxyId === id)) {
      throw new ApiError("PROFILE_LOCKED", "代理仍被环境使用");
    }
    this.proxies = this.proxies.filter((proxy) => proxy.id !== id);
  }

  async listDiagnostics(profileId?: string): Promise<DiagnosticReport[]> {
    return this.diagnostics
      .filter((report) => !profileId || report.profileId === profileId)
      .map((report) => ({ ...report, items: [...report.items], externalEndpoints: [...report.externalEndpoints] }));
  }

  async runDiagnostic(profileId: string, external = false): Promise<DiagnosticReport> {
    const profile = this.findProfile(profileId);
    const preset = DEFAULT_PRESETS.find((item) => item.id === profile.presetId) ?? DEFAULT_PRESETS[0];
    const report: DiagnosticReport = {
      profileId,
      checkedAt: now(),
      overall: "warning",
      externalEndpoints: external ? ["http://checkip.amazonaws.com/"] : [],
      items: [
        {
          key: "storage",
          label: "Profile 存储隔离",
          actual: "独立 User Data Dir",
          expected: "每个环境独立",
          scope: "isolated",
          status: "pass",
          explanation: "Cookie、缓存和站点存储由独立 Chromium Profile 保存。",
        },
        {
          key: "proxy",
          label: "代理配置",
          actual: profile.proxyId ? "已绑定代理" : "系统网络",
          expected: "按环境配置",
          scope: "configurable",
          status: profile.proxyId ? "pass" : "warning",
          explanation: profile.proxyId
            ? "运行时会通过每会话代理通道连接。"
            : "没有绑定代理，出口地址可能与其他环境相同。",
        },
        {
          key: "host-fingerprint",
          label: "Canvas / WebGL / GPU",
          actual: "使用主机原生值",
          expected: "首版不修改",
          scope: "host-shared",
          status: "warning",
          explanation: `“${preset.name}”预设不会伪造底层设备指纹；多开不等于多台设备。`,
        },
      ],
    };
    this.diagnostics = [report, ...this.diagnostics.filter((item) => item.profileId !== profileId)];
    profile.diagnostic = {
      overall: report.overall,
      checkedAt: report.checkedAt,
      issueCount: report.items.filter((item) => item.status !== "pass").length,
    };
    return { ...report, items: [...report.items], externalEndpoints: [...report.externalEndpoints] };
  }

  async listBackups(): Promise<BackupJob[]> {
    return this.backups.map((backup) => ({ ...backup }));
  }

  async exportBackup(
    profileId: string,
    _destinationName: string,
    includeProxySecret: boolean,
    _password: string,
  ): Promise<BackupJob> {
    const profile = this.findProfile(profileId);
    const job: BackupJob = {
      id: newId("backup"),
      profileId,
      profileName: profile.name,
      status: "completed",
      createdAt: now(),
      note: includeProxySecret
        ? "演示模式：正式实现会在导出前再次确认代理凭据。"
        : "演示模式：未导出代理凭据。",
    };
    this.backups = [job, ...this.backups];
    return { ...job };
  }

  async importBackup(_sourceName: string, _password: string): Promise<Profile> {
    throw new ApiError("CAPABILITY_UNSUPPORTED", "演示模式不支持恢复导入");
  }

  async listRuntimes(): Promise<BrowserRuntime[]> {
    return runtimes.map((runtime) => ({ ...runtime }));
  }

  async getSettings(): Promise<Settings> {
    return { ...this.settings };
  }

  async updateSettings(changes: Partial<Settings>): Promise<Settings> {
    if (changes.maxConcurrentSessions !== undefined) {
      const value = Math.round(changes.maxConcurrentSessions);
      if (value < 1 || value > 10) {
        throw new ApiError("VALIDATION_FAILED", "并发上限必须在 1 到 10 之间");
      }
      changes.maxConcurrentSessions = value;
    }
    this.settings = { ...this.settings, ...changes, telemetryEnabled: false };
    return { ...this.settings };
  }

  private findProfile(id: string): Profile {
    const profile = this.profiles.find((item) => item.id === id);
    if (!profile) throw new ApiError("NOT_FOUND", "找不到浏览器环境");
    return profile;
  }
}
