import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { createApiClient } from "./api/client";
import { isTauriRuntime } from "./api/tauri";
import {
  ApiError,
  DEFAULT_PRESETS,
  type ApiClient,
  type BackupJob,
  type BackupPhase,
  type BrowserRuntime,
  type DeviceMemoryGb,
  type DiagnosticReport,
  type EnvironmentPreset,
  type FingerprintPreview,
  type FingerprintScope,
  type LegacyMigrationInspection,
  type LegacyMigrationRecord,
  type MoreLoginPreview,
  type MoreLoginImportResult,
  type PersonaConfig,
  type PlatformKind,
  type Profile,
  type ProfileInput,
  type ProfileStatus,
  type ProxyKind,
  type ProxySummary,
  type SeedableNoisePersonaConfig,
  type Settings,
} from "./types";

type ViewKey = "profiles" | "proxies" | "diagnostics" | "backups" | "settings";

const viewMeta: Record<ViewKey, { label: string; hint: string; icon: string }> = {
  profiles: { label: "环境", hint: "隔离的浏览器身份", icon: "▦" },
  proxies: { label: "代理", hint: "按环境绑定网络", icon: "⌁" },
  diagnostics: { label: "诊断", hint: "查看实际生效属性", icon: "⌁" },
  backups: { label: "备份", hint: "加密归档与恢复", icon: "↥" },
  settings: { label: "设置", hint: "安全与运行时", icon: "⚙" },
};

const statusMeta: Record<ProfileStatus, { label: string; tone: string }> = {
  stopped: { label: "已停止", tone: "neutral" },
  queued: { label: "排队中", tone: "amber" },
  starting: { label: "启动中", tone: "blue" },
  running: { label: "运行中", tone: "green" },
  stopping: { label: "停止中", tone: "blue" },
  failed: { label: "异常", tone: "red" },
  archived: { label: "已归档", tone: "muted" },
};

const scopeMeta: Record<FingerprintScope, { label: string; className: string }> = {
  isolated: { label: "独立隔离", className: "scope-isolated" },
  configurable: { label: "可配置", className: "scope-configurable" },
  "host-shared": { label: "主机共享", className: "scope-shared" },
};

function formatDate(value?: string): string {
  if (!value) return "—";
  const timestamp = /^\d+$/.test(value) ? Number(value) : value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestamp));
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let size = value / 1024;
  let index = 0;
  while (size >= 1024 && index < units.length - 1) { size /= 1024; index += 1; }
  return `${size.toFixed(size >= 10 ? 1 : 2)} ${units[index]}`;
}

// ── v0.2 SeedableNoise persona helpers ──────────────────────────────────────

const DEFAULT_SEEDABLE: SeedableNoisePersonaConfig = {
  seed: "",
  platform: "windows",
  hardwareConcurrency: 8,
  deviceMemory: "8",
  touchPoints: 0,
  screenWidth: 1920,
  screenHeight: 1080,
  colorDepth: 24,
  deviceScaleFactor: 1,
  webglVendor: "",
  webglRenderer: "",
  spoofCanvas: false,
  spoofAudio: false,
};

const PLATFORM_OPTIONS: { value: PlatformKind; label: string }[] = [
  { value: "windows", label: "Windows" },
  { value: "mac_os", label: "macOS" },
  { value: "linux", label: "Linux" },
];

const MEMORY_OPTIONS: { value: DeviceMemoryGb; label: string }[] = [
  { value: "0.25", label: "0.25 GB" },
  { value: "0.5", label: "0.5 GB" },
  { value: "1", label: "1 GB" },
  { value: "2", label: "2 GB" },
  { value: "4", label: "4 GB" },
  { value: "8", label: "8 GB" },
];

const SCALE_OPTIONS = [1, 1.25, 1.5, 2, 2.5, 3];

function randomSeed(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function deviceMemoryLabel(v: DeviceMemoryGb): string {
  const found = MEMORY_OPTIONS.find((option) => option.value === v);
  return found?.label ?? v;
}

function migrationStatusLabel(status: LegacyMigrationRecord["status"]): string {
  return { running: "进行中", completed: "已完成", failed: "失败", rolled_back: "已回滚" }[status];
}

function getErrorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error) return error.message;
  return "操作失败，请稍后重试";
}

function App() {
  const apiRef = useRef<ApiClient>(createApiClient());
  const [activeView, setActiveView] = useState<ViewKey>("profiles");
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [proxies, setProxies] = useState<ProxySummary[]>([]);
  const [diagnostics, setDiagnostics] = useState<DiagnosticReport[]>([]);
  const [backups, setBackups] = useState<BackupJob[]>([]);
  const [runtimes, setRuntimes] = useState<BrowserRuntime[]>([]);
  const [settings, setSettings] = useState<Settings | undefined>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [showCreate, setShowCreate] = useState(false);
  const [showMoreLoginImport, setShowMoreLoginImport] = useState(false);
  const [editingProfile, setEditingProfile] = useState<Profile | undefined>();
  const [showArchived, setShowArchived] = useState(false);
  const [search, setSearch] = useState("");

  const backupRequest = useRef(0);

  const refresh = useCallback(async () => {
    const request = ++backupRequest.current;
    setLoading(true);
    setError("");
    const results = await Promise.allSettled([
      apiRef.current.listProfiles(true),
      apiRef.current.listProxies(),
      apiRef.current.listDiagnostics(),
      apiRef.current.listBackups(),
      apiRef.current.listRuntimes(),
      apiRef.current.getSettings(),
    ]);
    const failures: string[] = [];
    const apply = <T,>(
      result: PromiseSettledResult<T>,
      setValue: (value: T) => void,
      label: string,
    ) => {
      if (result.status === "fulfilled") setValue(result.value);
      else failures.push(`${label}：${getErrorMessage(result.reason)}`);
    };
    apply(results[0], setProfiles, "环境");
    apply(results[1], setProxies, "代理");
    apply(results[2], setDiagnostics, "诊断");
    if (request === backupRequest.current) apply(results[3], setBackups, "备份");
    apply(results[4], setRuntimes, "浏览器运行时");
    apply(results[5], setSettings, "设置");
    if (failures.length > 0) setError(failures.join("；"));
    setLoading(false);
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (activeView !== "backups") return;
    let disposed = false;
    let timer: number | undefined;
    const poll = async () => {
      const request = ++backupRequest.current;
      try {
        const jobs = await apiRef.current.listBackups();
        if (!disposed && request === backupRequest.current) setBackups(jobs);
      } catch (cause) {
        if (!disposed && request === backupRequest.current) setError(`备份：${getErrorMessage(cause)}`);
      } finally {
        if (!disposed) timer = window.setTimeout(() => void poll(), 750);
      }
    };
    void poll();
    return () => { disposed = true; window.clearTimeout(timer); };
  }, [activeView]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("brows://session", () => {
      if (!disposed) void refresh();
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    }).catch(() => {
      // Event delivery is an optimization; periodic/manual refresh remains the
      // fallback if a host version does not expose the event bridge.
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(""), 4200);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const runningCount = profiles.filter((profile) => profile.status === "running").length;
  const queuedCount = profiles.filter((profile) => profile.status === "queued").length;
  const issueCount = profiles.filter(
    (profile) => profile.diagnostic.overall === "warning" || profile.diagnostic.overall === "fail",
  ).length;

  const filteredProfiles = useMemo(() => {
    const query = search.trim().toLowerCase();
    return profiles.filter((profile) => {
      if (!showArchived && profile.status === "archived") return false;
      return !query || [profile.name, ...profile.tags].some((value) => value.toLowerCase().includes(query));
    });
  }, [profiles, search, showArchived]);

  const notify = (message: string) => {
    setError("");
    setNotice(message);
  };

  const runAction = async (action: () => Promise<void>, success: string) => {
    setError("");
    try {
      await action();
      await refresh();
      notify(success);
    } catch (cause) {
      setError(getErrorMessage(cause));
    }
  };

  const handleCreate = async (input: ProfileInput) => {
    await runAction(
      async () => {
        await apiRef.current.createProfile(input);
        setShowCreate(false);
      },
      "环境已创建，可以开始多开。",
    );
  };

  const handleUpdate = async (input: ProfileInput) => {
    if (!editingProfile) return;
    await runAction(
      async () => {
        await apiRef.current.updateProfile(
          editingProfile.id,
          editingProfile.revision,
          input,
        );
        setEditingProfile(undefined);
      },
      "环境配置已更新，将在下次启动时生效。",
    );
  };

  const content = (() => {
    switch (activeView) {
      case "proxies":
        return (
          <ProxyView
            api={apiRef.current}
            proxies={proxies}
            onRefresh={refresh}
            onError={setError}
            onNotice={notify}
          />
        );
      case "diagnostics":
        return (
          <DiagnosticsView
            api={apiRef.current}
            profiles={profiles}
            reports={diagnostics}
            onRefresh={refresh}
            onError={setError}
            onNotice={notify}
          />
        );
      case "backups":
        return (
          <BackupsView
            api={apiRef.current}
            profiles={profiles}
            backups={backups}
            onRefresh={refresh}
            onError={setError}
            onNotice={notify}
          />
        );
      case "settings":
        return (
          <SettingsView
            api={apiRef.current}
            settings={settings}
            runtimes={runtimes}
            onRefresh={refresh}
            onError={setError}
            onNotice={notify}
          />
        );
      case "profiles":
      default:
        return (
          <ProfilesView
            api={apiRef.current}
            profiles={filteredProfiles}
            allProfiles={profiles}
            presets={DEFAULT_PRESETS}
            proxies={proxies}
            runtimes={runtimes}
            search={search}
            showArchived={showArchived}
            onSearch={setSearch}
            onShowArchived={setShowArchived}
            onCreate={() => setShowCreate(true)}
            onImport={() => setShowMoreLoginImport(true)}
            onEdit={setEditingProfile}
            onRefresh={refresh}
            onError={setError}
            onNotice={notify}
          />
        );
    }
  })();

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-lockup">
          <div className="brand-mark" aria-hidden="true">
            B
          </div>
          <div>
            <div className="brand-name">brows</div>
            <div className="brand-subtitle">隔离浏览器环境</div>
          </div>
        </div>

        <div className="workspace-switcher" role="button" tabIndex={0} aria-label="当前工作区">
          <span className="workspace-dot" aria-hidden="true" />
          <span className="workspace-copy">
            <strong>本地工作区</strong>
            <small>仅此设备 · 零遥测</small>
          </span>
          <span className="chevron" aria-hidden="true">
            ⌄
          </span>
        </div>

        <nav className="primary-nav" aria-label="主导航">
          <div className="nav-section-label">工作区</div>
          {(Object.keys(viewMeta) as ViewKey[]).map((key) => (
            <button
              className={`nav-item ${activeView === key ? "active" : ""}`}
              key={key}
              type="button"
              onClick={() => setActiveView(key)}
            >
              <span className="nav-icon" aria-hidden="true">
                {viewMeta[key].icon}
              </span>
              <span className="nav-copy">
                <strong>{viewMeta[key].label}</strong>
                <small>{viewMeta[key].hint}</small>
              </span>
              {key === "profiles" && profiles.length > 0 && <span className="nav-count">{profiles.length}</span>}
              {key === "diagnostics" && issueCount > 0 && <span className="nav-alert">{issueCount}</span>}
            </button>
          ))}
        </nav>

        <div className="sidebar-footer">
          <div className="security-chip">
            <span className="security-icon" aria-hidden="true">
              ✓
            </span>
            <span>
              <strong>安全基线</strong>
              <small>沙箱 · 隔离 · 无遥测</small>
            </span>
          </div>
          <div className="version-line">brows v0.1 · 本地原型</div>
        </div>
      </aside>

      <main className="main-area">
        <header className="topbar">
          <div className="breadcrumb">
            <span>本地工作区</span>
            <span className="breadcrumb-separator">/</span>
            <strong>{viewMeta[activeView].label}</strong>
          </div>
          <div className="topbar-actions">
            <span className="connection-status">
              <span className="connection-dot" aria-hidden="true" /> 本地安全连接
            </span>
            <button className="icon-button" type="button" title="刷新" aria-label="刷新" onClick={() => void refresh()}>
              ↻
            </button>
            <button className="avatar-button" type="button" aria-label="用户菜单">
              本
            </button>
          </div>
        </header>

        <div className="content-wrap">
          {error && (
            <div className="alert-banner error" role="alert">
              <span aria-hidden="true">!</span>
              <span>{error}</span>
              <button type="button" onClick={() => setError("")} aria-label="关闭错误提示">
                ×
              </button>
            </div>
          )}
          {notice && (
            <div className="alert-banner success" role="status">
              <span aria-hidden="true">✓</span>
              <span>{notice}</span>
              <button type="button" onClick={() => setNotice("")} aria-label="关闭提示">
                ×
              </button>
            </div>
          )}

          {loading && profiles.length === 0 ? (
            <LoadingState />
          ) : (
            <>
              {activeView === "profiles" && (
                <div className="summary-row" aria-label="环境概览">
                  <SummaryCard label="全部环境" value={profiles.length} detail="已保存的隔离身份" accent="purple" />
                  <SummaryCard label="正在运行" value={runningCount} detail={`并发上限 ${settings?.maxConcurrentSessions ?? 10}`} accent="green" />
                  <SummaryCard label="排队中" value={queuedCount} detail="等待运行配额" accent="amber" />
                  <SummaryCard label="需要关注" value={issueCount} detail="诊断警告或失败" accent="red" />
                </div>
              )}
              {content}
            </>
          )}
        </div>
      </main>

      {showCreate && (
        <ProfileModal
          api={apiRef.current}
          presets={DEFAULT_PRESETS}
          proxies={proxies}
          runtimes={runtimes}
          onClose={() => setShowCreate(false)}
          onSubmit={(input) => void handleCreate(input)}
        />
      )}
      {showMoreLoginImport && (
        <MoreLoginImportModal
          api={apiRef.current}
          profiles={profiles}
          onClose={() => setShowMoreLoginImport(false)}
          onImported={async (count) => {
            await refresh();
            notify(`已安全导入 ${count} 个环境；敏感字段没有迁移。`);
          }}
        />
      )}
      {editingProfile && (
        <ProfileModal
          api={apiRef.current}
          initial={editingProfile}
          presets={DEFAULT_PRESETS}
          proxies={proxies}
          runtimes={runtimes}
          onClose={() => setEditingProfile(undefined)}
          onSubmit={(input) => void handleUpdate(input)}
        />
      )}
    </div>
  );
}

function SummaryCard({
  label,
  value,
  detail,
  accent,
}: {
  label: string;
  value: number;
  detail: string;
  accent: string;
}) {
  return (
    <div className={`summary-card accent-${accent}`}>
      <div className="summary-label">{label}</div>
      <div className="summary-value">{value}</div>
      <div className="summary-detail">{detail}</div>
    </div>
  );
}

function LoadingState() {
  return (
    <div className="empty-state loading-state">
      <div className="spinner" aria-hidden="true" />
      <strong>正在读取本地环境…</strong>
      <span>管理器不会访问远程服务。</span>
    </div>
  );
}

function ProfilesView({
  api,
  profiles,
  allProfiles,
  presets,
  proxies,
  runtimes,
  search,
  showArchived,
  onSearch,
  onShowArchived,
  onCreate,
  onImport,
  onEdit,
  onRefresh,
  onError,
  onNotice,
}: {
  api: ApiClient;
  profiles: Profile[];
  allProfiles: Profile[];
  presets: EnvironmentPreset[];
  proxies: ProxySummary[];
  runtimes: BrowserRuntime[];
  search: string;
  showArchived: boolean;
  onSearch(value: string): void;
  onShowArchived(value: boolean): void;
  onCreate(): void;
  onImport(): void;
  onEdit(profile: Profile): void;
  onRefresh(): Promise<void>;
  onError(message: string): void;
  onNotice(message: string): void;
}) {
  const presetById = useMemo(() => new Map(presets.map((preset) => [preset.id, preset])), [presets]);
  const proxyById = useMemo(() => new Map(proxies.map((proxy) => [proxy.id, proxy])), [proxies]);
  const runtimeById = useMemo(() => new Map(runtimes.map((runtime) => [runtime.id, runtime])), [runtimes]);

  const run = async (action: () => Promise<unknown>, success: string) => {
    try {
      await action();
      await onRefresh();
      onNotice(success);
    } catch (cause) {
      onError(getErrorMessage(cause));
    }
  };

  return (
    <section className="view-section">
      <div className="page-heading">
        <div>
          <div className="eyebrow">PROFILE MANAGER</div>
          <h1>浏览器环境</h1>
          <p>每个环境都有独立的 Cookie、缓存、代理和登录状态，可安全地同时运行。</p>
        </div>
        <div className="heading-actions">
          <button className="secondary-button" type="button" onClick={onImport}>从 MoreLogin 导入</button>
          <button className="primary-button" type="button" onClick={onCreate}>
            <span aria-hidden="true">＋</span> 新建环境
          </button>
        </div>
      </div>

      <div className="notice-card">
        <div className="notice-card-icon" aria-hidden="true">
          ◈
        </div>
        <div>
          <strong>多开隔离 ≠ 完整设备指纹伪装</strong>
          <p>首版保持 Chromium 和系统的真实底层属性。使用不同代理可以区分出口网络，但 Canvas、WebGL、GPU 等主机特征默认共享。</p>
        </div>
        <button className="text-button" type="button" onClick={() => onNotice("可在“诊断”中查看每项属性的隔离范围。")}>查看说明 →</button>
      </div>

      <div className="toolbar">
        <label className="search-box">
          <span aria-hidden="true">⌕</span>
          <input
            type="search"
            value={search}
            onChange={(event) => onSearch(event.target.value)}
            placeholder="搜索名称或标签"
            aria-label="搜索环境"
          />
          {search && (
            <button type="button" className="clear-search" aria-label="清除搜索" onClick={() => onSearch("")}>
              ×
            </button>
          )}
        </label>
        <div className="toolbar-spacer" />
        <label className="archive-filter">
          <input
            type="checkbox"
            checked={showArchived}
            onChange={(event) => onShowArchived(event.target.checked)}
          />
          <span>显示已归档</span>
        </label>
        <span className="result-count">显示 {profiles.length} / {allProfiles.length}</span>
        <button className="secondary-button" type="button" onClick={() => onNotice("批量启动将在调度器模块中提供。")}>
          批量操作 <span aria-hidden="true">⌄</span>
        </button>
      </div>

      {profiles.length === 0 ? (
        <div className="empty-state">
          <div className="empty-icon" aria-hidden="true">⌘</div>
          <strong>{search ? "没有匹配的环境" : "还没有浏览器环境"}</strong>
          <span>{search ? "尝试更换名称或标签。" : "创建第一个环境，开始隔离多开。"}</span>
          {!search && <button className="primary-button small" type="button" onClick={onCreate}>新建环境</button>}
        </div>
      ) : (
        <div className="profile-grid">
          {profiles.map((profile) => {
            const preset = presetById.get(profile.presetId);
            const proxy = profile.proxyId ? proxyById.get(profile.proxyId) : undefined;
            const runtime = runtimeById.get(profile.runtimeId);
            return (
              <ProfileCard
                key={profile.id}
                profile={profile}
                preset={preset}
                proxy={proxy}
                runtime={runtime}
                onLaunch={() => void run(() => api.launchSession(profile.id), "环境已启动。")}
                onStop={() => void run(() => api.stopSession(profile.id), "环境已停止。")}
                onEdit={() => onEdit(profile)}
                onClone={() => void run(() => api.cloneProfile(profile.id), "环境副本已创建。")}
                onArchive={() => void run(() => api.archiveProfile(profile.id, profile.revision), "环境已归档。")}
                onRestore={() => void run(() => api.restoreProfile(profile.id, profile.revision), "环境已恢复。")}
              />
            );
          })}
        </div>
      )}
    </section>
  );
}

function ProfileCard({
  profile,
  preset,
  proxy,
  runtime,
  onLaunch,
  onStop,
  onEdit,
  onClone,
  onArchive,
  onRestore,
}: {
  profile: Profile;
  preset?: EnvironmentPreset;
  proxy?: ProxySummary;
  runtime?: BrowserRuntime;
  onLaunch(): void;
  onStop(): void;
  onEdit(): void;
  onClone(): void;
  onArchive(): void;
  onRestore(): void;
}) {
  const status = statusMeta[profile.status];
  const canLaunch = profile.status === "stopped" || profile.status === "failed";
  const canStop = profile.status === "running" || profile.status === "starting" || profile.status === "queued";
  const canEdit = profile.status === "stopped" || profile.status === "failed";
  const canArchive = profile.status === "stopped" || profile.status === "failed";
  const isSeedable = profile.persona?.kind === "seedable_noise";
  const fingerprintBlocked = isSeedable && runtime?.fingerprintSupport !== "supported";
  const scope = scopeMeta[isSeedable ? "configurable" : preset?.fingerprintScope ?? "host-shared"];

  return (
    <article className={`profile-card status-${status.tone}`}>
      <div className="profile-card-head">
        <div className="profile-avatar" aria-hidden="true">{profile.name.trim().charAt(0).toUpperCase() || "B"}</div>
        <div className="profile-title-wrap">
          <h2 title={profile.name}>{profile.name}</h2>
          <span className="profile-id">{profile.id.slice(0, 18)}</span>
        </div>
        <span className={`status-pill ${status.tone}`}><span className="status-dot" />{status.label}</span>
      </div>

      <div className="tag-row">
        {profile.tags.slice(0, 3).map((tag) => <span className="tag" key={tag}>{tag}</span>)}
        <span className={`scope-badge ${scope.className}`}>{scope.label}</span>
      </div>

      <div className="profile-details">
        <DetailRow label="代理" value={proxy ? `${proxy.name} · ${proxy.kind.toUpperCase()}` : "系统网络"} />
        <DetailRow label="浏览器" value={runtime ? `${runtime.name} ${runtime.version}` : "待选择运行时"} />
        <DetailRow label="指纹" value={isSeedable ? `${fingerprintSupportLabel(runtime)}${fingerprintBlocked ? " · 启动受限，请到设置检测" : " · 非完整 anti-detect"}` : "系统原生 · 不要求指纹扩展"} />
        <DetailRow label="上次使用" value={formatDate(profile.lastUsedAt)} />
      </div>

      <div className="profile-card-footer">
        <span className={`diagnostic-mini ${profile.diagnostic.overall}`}>
          <span aria-hidden="true">{profile.diagnostic.overall === "pass" ? "✓" : profile.diagnostic.overall === "not-run" ? "○" : "!"}</span>
          {profile.diagnostic.overall === "not-run" ? "未诊断" : `诊断 ${profile.diagnostic.issueCount ? `${profile.diagnostic.issueCount} 项提示` : "通过"}`}
        </span>
        <div className="card-actions">
          {profile.status === "archived" && <button className="card-action primary" type="button" onClick={onRestore}>恢复</button>}
          {canLaunch && <button className="card-action primary" type="button" disabled={fingerprintBlocked} title={fingerprintBlocked ? "SeedableNoise 需要运行时指纹加载检测通过。请到设置执行检测，或编辑为系统原生。" : undefined} onClick={onLaunch}>启动</button>}
          {canStop && <button className="card-action stop" type="button" onClick={onStop}>停止</button>}
          {canEdit && <button className="card-action" type="button" onClick={onEdit}>编辑</button>}
          <button className="card-action" type="button" onClick={onClone}>克隆</button>
          {canArchive && <button className="card-action" type="button" onClick={onArchive}>归档</button>}
        </div>
      </div>
    </article>
  );
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return <div className="detail-row"><span>{label}</span><strong title={value}>{value}</strong></div>;
}

function ProfileModal({
  initial,
  presets,
  proxies,
  runtimes,
  api,
  onClose,
  onSubmit,
}: {
  initial?: Profile;
  presets: EnvironmentPreset[];
  proxies: ProxySummary[];
  runtimes: BrowserRuntime[];
  api: ApiClient;
  onClose(): void;
  onSubmit(input: ProfileInput): void;
}) {
  const [name, setName] = useState(initial?.name ?? "");
  const [tags, setTags] = useState(initial?.tags.join(", ") ?? "");
  const [presetId, setPresetId] = useState(initial?.presetId ?? presets[0]?.id ?? "system-native");
  const [proxyId, setProxyId] = useState(initial?.proxyId ?? "");
  const [runtimeId, setRuntimeId] = useState(initial?.runtimeId ?? runtimes.find((runtime) => runtime.active)?.id ?? runtimes[0]?.id ?? "");
  const [persona, setPersona] = useState<PersonaConfig>(initial?.persona ?? { kind: "system_native" });
  const [languagesText, setLanguagesText] = useState((initial?.acceptLanguages ?? ["zh-CN", "zh"]).join(", "));
  const [windowWidth, setWindowWidth] = useState(initial?.windowWidth ?? 1280);
  const [windowHeight, setWindowHeight] = useState(initial?.windowHeight ?? 800);
  const [formError, setFormError] = useState("");
  const acceptLanguages = useMemo(() => languagesText.split(",").map((language) => language.trim()).filter(Boolean), [languagesText]);
  const selectedRuntime = runtimes.find((runtime) => runtime.id === runtimeId);

  // Runtime discovery is asynchronous. Keep the form selection in sync when
  // the list arrives after the modal has already been mounted, while
  // preserving an existing profile's pinned runtime whenever it is present.
  useEffect(() => {
    if (runtimeId && runtimes.some((runtime) => runtime.id === runtimeId)) return;
    setRuntimeId(
      runtimes.find((runtime) => runtime.active && runtime.trusted)?.id ??
        runtimes.find((runtime) => runtime.trusted)?.id ??
        "",
    );
  }, [runtimes, runtimeId]);

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const cleanName = name.trim();
    if (cleanName.length < 1 || cleanName.length > 64) {
      setFormError("名称长度需要在 1 到 64 个字符之间。");
      return;
    }
    if (!selectedRuntime || !selectedRuntime.trusted) {
      setFormError("请先安装或选择一个可信的浏览器运行时。");
      return;
    }
    if (acceptLanguages.length === 0 || acceptLanguages.some((language) => language.length > 32 || !/^[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$/.test(language))) {
      setFormError("请输入逗号分隔的语言标签，例如 zh-CN, zh 或 en-US, en。");
      return;
    }
    if (!Number.isInteger(windowWidth) || !Number.isInteger(windowHeight) || windowWidth < 320 || windowHeight < 240 || windowWidth > 0xffffffff || windowHeight > 0xffffffff) {
      setFormError("窗口尺寸必须为整数，且宽度至少 320、高度至少 240。");
      return;
    }
    setFormError("");
    onSubmit({
      name: cleanName,
      tags: tags.split(",").map((tag) => tag.trim()).filter(Boolean).slice(0, 8),
      presetId,
      proxyId: proxyId || undefined,
      runtimeId,
      persona,
      acceptLanguages: [...acceptLanguages],
      windowWidth,
      windowHeight,
    });
  };

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <section className="modal" role="dialog" aria-modal="true" aria-labelledby="profile-modal-title">
        <div className="modal-head">
          <div><div className="eyebrow">{initial ? "EDIT PROFILE" : "NEW PROFILE"}</div><h2 id="profile-modal-title">{initial ? "编辑浏览器环境" : "新建浏览器环境"}</h2></div>
          <button className="icon-button" type="button" aria-label="关闭" onClick={onClose}>×</button>
        </div>
        <p className="modal-intro">创建后会分配独立的 User Data Dir。账号密码不会进入 Brows 数据库。</p>
        <form onSubmit={submit}>
          <label className="field-label" htmlFor="profile-name">环境名称<span>*</span></label>
          <input id="profile-name" className="text-input" autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：研究环境 · 东京" maxLength={64} />

          <label className="field-label" htmlFor="profile-tags">标签</label>
          <input id="profile-tags" className="text-input" value={tags} onChange={(event) => setTags(event.target.value)} placeholder="用逗号分隔，例如：研究, 东京" />

          <div className="form-grid">
            <div>
              <label className="field-label" htmlFor="profile-preset">环境预设</label>
              <select id="profile-preset" className="text-input" value={presetId} onChange={(event) => setPresetId(event.target.value)}>
                {presets.map((preset) => <option value={preset.id} key={preset.id}>{preset.name}</option>)}
              </select>
            </div>
            <div>
              <label className="field-label" htmlFor="profile-proxy">代理</label>
              <select id="profile-proxy" className="text-input" value={proxyId} onChange={(event) => setProxyId(event.target.value)}>
                <option value="">系统网络（不推荐用于跨地区测试）</option>
                {proxies.map((proxy) => <option value={proxy.id} key={proxy.id}>{proxy.name} · {proxy.kind.toUpperCase()}</option>)}
              </select>
            </div>
          </div>

          <label className="field-label" htmlFor="profile-runtime">浏览器运行时</label>
          <select id="profile-runtime" className="text-input" value={runtimeId} disabled={runtimes.length === 0} onChange={(event) => setRuntimeId(event.target.value)}>
            {runtimes.length === 0 && <option value="">没有可用运行时</option>}
            {runtimes.map((runtime) => <option value={runtime.id} key={runtime.id} disabled={!runtime.trusted}>{runtime.name} {runtime.version}{runtime.trusted ? "" : " · 未验证"}</option>)}
          </select>

          <label className="field-label" htmlFor="profile-languages">语言（按优先级，用逗号分隔）</label>
          <input id="profile-languages" className="text-input" required value={languagesText} onChange={(event) => setLanguagesText(event.target.value)} placeholder="zh-CN, zh" />
          <div className="form-grid">
            <div><label className="field-label" htmlFor="profile-window-w">启动窗口宽度</label><input id="profile-window-w" className="text-input" type="number" required min={320} max={0xffffffff} step={1} value={windowWidth} onChange={(event) => setWindowWidth(Number(event.target.value))} /></div>
            <div><label className="field-label" htmlFor="profile-window-h">启动窗口高度</label><input id="profile-window-h" className="text-input" type="number" required min={240} max={0xffffffff} step={1} value={windowHeight} onChange={(event) => setWindowHeight(Number(event.target.value))} /></div>
          </div>

          <PersonaEditor value={persona} onChange={setPersona} api={api} runtimeId={runtimeId} acceptLanguages={acceptLanguages} windowWidth={windowWidth} windowHeight={windowHeight} />

          <div className="form-info" role="status">
            <span aria-hidden="true">i</span>
            <div>运行时指纹支持：{fingerprintSupportLabel(selectedRuntime)}。签名可信不代表指纹支持。
              {persona.kind === "seedable_noise"
                ? selectedRuntime?.fingerprintSupport === "supported"
                  ? " 加载检测通过仅说明本地探针可注入，仍非完整 anti-detect。"
                  : " 可保存指纹草稿，但启动前必须在设置中通过加载检测；也可改为系统原生。"
                : " 系统原生不受指纹扩展支持限制。"}
            </div>
          </div>

          {formError && <div className="form-error" role="alert">{formError}</div>}
          <div className="modal-actions"><button className="secondary-button" type="button" onClick={onClose}>取消</button><button className="primary-button" type="submit">{initial ? "保存修改" : "创建环境"}</button></div>
        </form>
      </section>
    </div>
  );
}

// ── v0.2 PersonaEditor ──────────────────────────────────────────────────────

function PersonaEditor({
  value,
  onChange,
  api,
  runtimeId,
  acceptLanguages,
  windowWidth,
  windowHeight,
}: {
  value: PersonaConfig;
  onChange(next: PersonaConfig): void;
  api: ApiClient;
  runtimeId: string;
  acceptLanguages: string[];
  windowWidth: number;
  windowHeight: number;
}) {
  const lastSeedable = useRef<SeedableNoisePersonaConfig | undefined>(value.seedable);
  if (value.kind === "seedable_noise" && value.seedable) lastSeedable.current = value.seedable;
  return (
    <fieldset className="persona-editor">
      <legend className="field-label">指纹 Persona</legend>
      <div className="persona-tabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={value.kind === "system_native"}
          className={`persona-tab ${value.kind === "system_native" ? "active" : ""}`}
          onClick={() => value.kind !== "system_native" && onChange({ kind: "system_native" })}
        >
          系统原生
          <small>v0.1 · 主机共享</small>
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={value.kind === "seedable_noise"}
          className={`persona-tab ${value.kind === "seedable_noise" ? "active" : ""}`}
          onClick={() => {
            if (value.kind === "seedable_noise") return;
            onChange({
              kind: "seedable_noise",
              seedable: lastSeedable.current ?? { ...DEFAULT_SEEDABLE, seed: randomSeed() },
            });
          }}
        >
          SeedableNoise
          <small>v0.2 · 可配置</small>
        </button>
      </div>

      {value.kind === "seedable_noise" && value.seedable && (
        <SeedableNoiseFields
          value={value.seedable}
          onChange={(seedable) => onChange({ kind: "seedable_noise", seedable })}
          api={api}
          runtimeId={runtimeId}
          acceptLanguages={acceptLanguages}
          windowWidth={windowWidth}
          windowHeight={windowHeight}
        />
      )}
    </fieldset>
  );
}

function SeedableNoiseFields({
  value,
  onChange,
  api,
  runtimeId,
  acceptLanguages,
  windowWidth,
  windowHeight,
}: {
  value: SeedableNoisePersonaConfig;
  onChange(next: SeedableNoisePersonaConfig): void;
  api: ApiClient;
  runtimeId: string;
  acceptLanguages: string[];
  windowWidth: number;
  windowHeight: number;
}) {
  const [preview, setPreview] = useState<{ key: string; result: FingerprintPreview } | undefined>();
  const [previewError, setPreviewError] = useState("");
  const [previewing, setPreviewing] = useState(false);
  const previewKey = JSON.stringify({ value, runtimeId, acceptLanguages, windowWidth, windowHeight });
  const currentKey = useRef(previewKey);
  currentKey.current = previewKey;
  const requestSequence = useRef(0);

  useEffect(() => {
    requestSequence.current += 1;
    setPreview(undefined);
    setPreviewError("");
    setPreviewing(false);
    return () => { requestSequence.current += 1; };
  }, [api, previewKey]);

  const requestPreview = useCallback(async () => {
    const sequence = ++requestSequence.current;
    const key = previewKey;
    const isCurrent = () => sequence === requestSequence.current && key === currentKey.current;
    setPreviewing(true);
    setPreview(undefined);
    setPreviewError("");
    try {
      const result = await api.fingerprintPreview({
        persona: {
          kind: "seedable_noise",
          seedable: value,
        },
        runtimeId,
        acceptLanguages: [...acceptLanguages],
        windowWidth,
        windowHeight,
      });
      if (isCurrent()) setPreview({ key, result });
    } catch (error) {
      if (isCurrent()) setPreviewError(getErrorMessage(error));
    } finally {
      if (isCurrent()) setPreviewing(false);
    }
  }, [api, value, runtimeId, acceptLanguages, windowWidth, windowHeight, previewKey]);

  return (
    <div className="seedable-noise-fields">
      <div className="form-info">
        <span aria-hidden="true">⚠</span>
        此配置依赖浏览器支持加载 MV3 扩展，生成预览不代表注入已生效。
        Canvas 读取 / 导出与离线 Audio 渲染可进行种子微扰；Client Hints、字体、真实 GPU 和 worker 等仍未完整覆盖，不代表完整 anti-detect。
      </div>

      <label className="field-label" htmlFor="persona-seed">种子</label>
      <div className="seed-row">
        <input
          id="persona-seed"
          className="text-input"
          value={value.seed}
          onChange={(event) => onChange({ ...value, seed: event.target.value })}
          placeholder="相同种子、配置和内核 → 相同派生结果"
          maxLength={128}
        />
        <button
          type="button"
          className="secondary-button"
          onClick={() => onChange({ ...value, seed: randomSeed() })}
        >
          随机生成
        </button>
      </div>

      <div className="form-grid">
        <div>
          <label className="field-label" htmlFor="persona-platform">平台</label>
          <select
            id="persona-platform"
            className="text-input"
            value={value.platform}
            onChange={(event) =>
              onChange({ ...value, platform: event.target.value as PlatformKind })
            }
          >
            {PLATFORM_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
        </div>
        <div>
          <label className="field-label" htmlFor="persona-mem">设备内存</label>
          <select
            id="persona-mem"
            className="text-input"
            value={value.deviceMemory}
            onChange={(event) =>
              onChange({ ...value, deviceMemory: event.target.value as DeviceMemoryGb })
            }
          >
            {MEMORY_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
        </div>
      </div>

      <div className="form-grid">
        <div>
          <label className="field-label" htmlFor="persona-cores">CPU 核心数</label>
          <input
            id="persona-cores"
            type="number"
            min={1}
            max={128}
            className="text-input"
            value={value.hardwareConcurrency}
            onChange={(event) =>
              onChange({ ...value, hardwareConcurrency: Math.max(1, Math.min(128, Number(event.target.value) || 1)) })
            }
          />
        </div>
        <div>
          <label className="field-label" htmlFor="persona-touch">触摸点</label>
          <input
            id="persona-touch"
            type="number"
            min={0}
            max={10}
            className="text-input"
            value={value.touchPoints}
            onChange={(event) =>
              onChange({ ...value, touchPoints: Math.max(0, Math.min(10, Number(event.target.value) || 0)) })
            }
          />
        </div>
      </div>

      <div className="form-grid">
        <div>
          <label className="field-label" htmlFor="persona-screen-w">屏幕宽度</label>
          <input
            id="persona-screen-w"
            type="number"
            min={320}
            className="text-input"
            value={value.screenWidth}
            onChange={(event) => onChange({ ...value, screenWidth: Number(event.target.value) || 320 })}
          />
        </div>
        <div>
          <label className="field-label" htmlFor="persona-screen-h">屏幕高度</label>
          <input
            id="persona-screen-h"
            type="number"
            min={240}
            className="text-input"
            value={value.screenHeight}
            onChange={(event) => onChange({ ...value, screenHeight: Number(event.target.value) || 240 })}
          />
        </div>
      </div>

      <div className="form-grid">
        <div>
          <label className="field-label" htmlFor="persona-color">颜色深度</label>
          <select
            id="persona-color"
            className="text-input"
            value={value.colorDepth}
            onChange={(event) => onChange({ ...value, colorDepth: Number(event.target.value) })}
          >
            <option value={16}>16 位</option>
            <option value={24}>24 位</option>
            <option value={30}>30 位</option>
            <option value={48}>48 位</option>
          </select>
        </div>
        <div>
          <label className="field-label" htmlFor="persona-scale">缩放因子</label>
          <select
            id="persona-scale"
            className="text-input"
            value={value.deviceScaleFactor}
            onChange={(event) =>
              onChange({ ...value, deviceScaleFactor: Number(event.target.value) })
            }
          >
            {SCALE_OPTIONS.map((s) => (
              <option key={s} value={s}>{s.toFixed(s === 1 ? 0 : 1)}×</option>
            ))}
          </select>
        </div>
      </div>

      <label className="field-label" htmlFor="persona-webgl-vendor">WebGL Vendor（留空 = 自动派生）</label>
      <input
        id="persona-webgl-vendor"
        className="text-input"
        value={value.webglVendor}
        onChange={(event) => onChange({ ...value, webglVendor: event.target.value })}
        placeholder="例：Google Inc. (NVIDIA)"
      />

      <label className="field-label" htmlFor="persona-webgl-renderer">WebGL Renderer（留空 = 自动派生）</label>
      <input
        id="persona-webgl-renderer"
        className="text-input"
        value={value.webglRenderer}
        onChange={(event) => onChange({ ...value, webglRenderer: event.target.value })}
        placeholder="例：ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 ...)"
      />

      <div className="form-toggle-row">
        <label className="toggle-row">
          <input
            type="checkbox"
            checked={value.spoofCanvas}
            onChange={(event) => onChange({ ...value, spoofCanvas: event.target.checked })}
          />
          <span>
            Canvas 读取 / 导出扰动
            <small>确定性像素微扰，不修改原始画布；仍可被检测</small>
          </span>
        </label>
        <label className="toggle-row">
          <input
            type="checkbox"
            checked={value.spoofAudio}
            onChange={(event) => onChange({ ...value, spoofAudio: event.target.checked })}
          />
          <span>
            离线 Audio 渲染微扰
            <small>只微扰离线 buffer，不改变真实采样率或实时音频</small>
          </span>
        </label>
      </div>

      <FingerprintPreviewPanel
        preview={preview?.key === previewKey ? preview.result : undefined}
        error={previewError}
        loading={previewing}
        onRequest={requestPreview}
      />
    </div>
  );
}

// ── v0.2 FingerprintPreviewPanel ────────────────────────────────────────────

function FingerprintPreviewPanel({
  preview,
  error,
  loading,
  onRequest,
}: {
  preview: FingerprintPreview | undefined;
  error: string;
  loading: boolean;
  onRequest(): void;
}) {
  return (
    <div className="fingerprint-preview">
      <div className="preview-head">
        <div>
          <div className="eyebrow">指纹配置预览（未观测）</div>
          <small>不启动浏览器；按所选内核和配置派生，实际覆盖需验证扩展加载</small>
        </div>
        <button
          type="button"
          className="primary-button"
          onClick={onRequest}
          disabled={loading}
        >
          {loading ? "生成中…" : "预览"}
        </button>
      </div>

      {error && <div className="form-error" role="alert">{error}</div>}

      {preview && (
        <div className="preview-body">
          <div className="preview-row">
            <span className="preview-label">内核</span>
            <code className="preview-value">{preview.runtimeId} · {preview.runtimeVersion}</code>
          </div>
          <div className="preview-row">
            <span className="preview-label">语言</span>
            <code className="preview-value">{preview.effectiveLanguages.join(", ")}</code>
          </div>
          <div className="preview-row">
            <span className="preview-label">User-Agent</span>
            <code className="preview-value">{preview.effectiveUserAgent}</code>
          </div>
          <div className="preview-row">
            <span className="preview-label">Platform</span>
            <code className="preview-value">{preview.effectivePlatform}</code>
          </div>
          <div className="preview-row">
            <span className="preview-label">WebGL Vendor</span>
            <code className="preview-value">{preview.effectiveWebglVendor}</code>
          </div>
          <div className="preview-row">
            <span className="preview-label">WebGL Renderer</span>
            <code className="preview-value">{preview.effectiveWebglRenderer}</code>
          </div>
          <div className="preview-meta">
            <span>CPU <strong>{preview.hardwareConcurrency}</strong> 核</span>
            <span>内存 <strong>{preview.deviceMemoryGb}</strong> GB</span>
            <span>窗口 <strong>{preview.effectiveWindowWidth}×{preview.effectiveWindowHeight}</strong></span>
            <span>屏幕 <strong>{preview.effectiveScreenWidth}×{preview.effectiveScreenHeight}</strong></span>
            <span>启动缩放 <strong>{preview.deviceScaleFactorHundredths / 100}×</strong></span>
          </div>
          {preview.declaredOnlyFields.length > 0 && (
            <div className="preview-warning">
              <strong>页面读取覆盖（需验证注入，非完整设备替换）</strong>
              <ul>
                {preview.declaredOnlyFields.map((field) => (
                  <li key={field}>{field}</li>
                ))}
              </ul>
            </div>
          )}
          {preview.hostSharedFields.length > 0 && (
            <div className="preview-warning">
              <strong>仍依赖主机 / 内核的信号</strong>
              <ul>
                {preview.hostSharedFields.map((field) => (
                  <li key={field}>{field}</li>
                ))}
              </ul>
            </div>
          )}
          {preview.runtimeWarnings.length > 0 && (
            <div className="preview-warning">
              <strong>兼容性与覆盖限制</strong>
              <ul>{preview.runtimeWarnings.map((warning, index) => <li key={`${index}:${warning}`}>{warning}</li>)}</ul>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function MoreLoginImportModal({
  api,
  profiles,
  onClose,
  onImported,
}: {
  api: ApiClient;
  profiles: Profile[];
  onClose(): void;
  onImported(count: number): Promise<void>;
}) {
  const [sourcePath, setSourcePath] = useState("");
  const [preview, setPreview] = useState<MoreLoginPreview>();
  const [importResult, setImportResult] = useState<MoreLoginImportResult>();
  const [targets, setTargets] = useState(profiles);
  const [sourceProfileId, setSourceProfileId] = useState("");
  const [targetProfileId, setTargetProfileId] = useState("");
  const [inspection, setInspection] = useState<LegacyMigrationInspection>();
  const [migration, setMigration] = useState<LegacyMigrationRecord>();
  const [history, setHistory] = useState<LegacyMigrationRecord[]>([]);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState("");

  useEffect(() => {
    void api.listLegacyMigrations().then(setHistory).catch(() => undefined);
  }, [api]);

  const inspect = async () => {
    if (!sourcePath.trim()) {
      setFormError("请输入 MoreLogin 导出的 XLSX 文件路径。");
      return;
    }
    setBusy(true);
    setFormError("");
    try {
      const result = await api.previewMoreLogin(sourcePath.trim());
      setPreview(result);
      setImportResult(undefined);
      setSourceProfileId(result.candidates[0]?.sourceProfileId ?? "");
      const matched = targets.find((profile) => profile.name === result.candidates[0]?.name);
      setTargetProfileId(matched?.id ?? targets[0]?.id ?? "");
      setInspection(undefined);
    } catch (cause) {
      setPreview(undefined);
      setFormError(getErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const importProfiles = async () => {
    if (!preview) return;
    setBusy(true);
    setFormError("");
    try {
      const result = await api.importMoreLogin(sourcePath.trim());
      setImportResult(result);
      await onImported(result.importedCount);
      const refreshed = await api.listProfiles();
      setTargets(refreshed);
      const candidate = preview.candidates.find((item) => item.sourceProfileId === sourceProfileId)
        ?? preview.candidates[0];
      const matched = refreshed.find((profile) => profile.name === candidate?.name);
      setTargetProfileId(matched?.id ?? refreshed[0]?.id ?? "");
      if (result.failures.length > 0) {
        setFormError(`已导入 ${result.importedCount} 个，另有 ${result.failures.length} 个失败。首个失败位于第 ${result.failures[0].rowNumber} 行：${result.failures[0].reason}`);
        return;
      }
    } catch (cause) {
      setFormError(getErrorMessage(cause));
    } finally {
      setBusy(false);
    }
  };

  const selectSource = (value: string) => {
    setSourceProfileId(value);
    setInspection(undefined);
    setMigration(undefined);
    const candidate = preview?.candidates.find((item) => item.sourceProfileId === value);
    const matched = targets.find((profile) => profile.name === candidate?.name);
    if (matched) setTargetProfileId(matched.id);
  };

  const inspectProfileData = async () => {
    if (!targetProfileId || !sourceProfileId) return;
    setBusy(true);
    setFormError("");
    try {
      setInspection(await api.inspectLegacyMigration(targetProfileId, sourceProfileId));
      setMigration(undefined);
    } catch (cause) {
      setInspection(undefined);
      setFormError(getErrorMessage(cause));
    } finally { setBusy(false); }
  };

  const executeMigration = async () => {
    if (!inspection || inspection.sourceMayBeRunning) return;
    setBusy(true);
    setFormError("");
    try {
      const result = await api.executeLegacyMigration(targetProfileId, sourceProfileId);
      setMigration(result);
      setHistory(await api.listLegacyMigrations());
    } catch (cause) {
      setFormError(getErrorMessage(cause));
      setHistory(await api.listLegacyMigrations().catch(() => history));
    } finally { setBusy(false); }
  };

  const rollback = async (id: number) => {
    setBusy(true);
    setFormError("");
    try {
      await api.rollbackLegacyMigration(id);
      setHistory(await api.listLegacyMigrations());
      if (migration?.id === id) setMigration(undefined);
    } catch (cause) { setFormError(getErrorMessage(cause)); }
    finally { setBusy(false); }
  };

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && !busy && onClose()}>
      <section className="modal import-modal" role="dialog" aria-modal="true" aria-labelledby="morelogin-import-title">
        <div className="modal-head">
          <div><div className="eyebrow">SAFE LEGACY IMPORT</div><h2 id="morelogin-import-title">从 MoreLogin 导入</h2></div>
          <button className="icon-button" type="button" aria-label="关闭" disabled={busy} onClick={onClose}>×</button>
        </div>
        <p className="modal-intro">先导入环境清单，再选择单个环境执行完整迁移。完整迁移会复制 Cookie、登录状态、历史和站点存储，并保留可回滚备份。</p>
        <label className="field-label" htmlFor="morelogin-source">MoreLogin 导出文件</label>
        <div className="import-source-row">
          <input id="morelogin-source" className="text-input" value={sourcePath} onChange={(event) => { setSourcePath(event.target.value); setPreview(undefined); }} placeholder="例如：D:\\export_profile（可省略 .xlsx）" />
          <button className="secondary-button" type="button" disabled={busy} onClick={() => void inspect()}>{busy && !preview ? "读取中…" : "安全预览"}</button>
        </div>
        {preview && (
          <div className="import-preview">
            <div className="import-summary"><strong>{preview.profileCount}</strong><span>个可导入环境</span><strong>{preview.sensitiveProfileCount}</strong><span>个含已忽略的敏感字段</span></div>
            <div className="import-notices">{preview.notices.map((notice) => <div key={notice}>• {notice}</div>)}</div>
            <div className="import-candidate-list">
              {preview.candidates.slice(0, 8).map((candidate) => (
                <div className="import-candidate" key={candidate.rowNumber}>
                  <div><strong>{candidate.name}</strong><small>第 {candidate.rowNumber} 行 · ID {candidate.sourceProfileId} · {candidate.tags.join(" / ") || "无标签"}</small></div>
                  <span>{candidate.warnings.length > 0 ? `${candidate.warnings.length} 项已忽略` : "可直接导入"}</span>
                </div>
              ))}
              {preview.profileCount > 8 && <div className="import-more">其余 {preview.profileCount - 8} 个环境将在确认后一起导入</div>}
            </div>
          </div>
        )}
        {importResult && (
          <div className={`import-result ${importResult.failures.length > 0 ? "warning" : "success"}`} role="status">
            <strong>{importResult.failures.length > 0 ? "导入部分完成" : "导入完成"}</strong>
            <span>成功 {importResult.importedCount} 个 · 跳过 {importResult.skippedCount} 个 · 失败 {importResult.failures.length} 个</span>
            <small>清单导入已完成。现在可以在下方选择一个环境进行完整数据迁移。</small>
          </div>
        )}
        {preview && (
          <div className="migration-panel">
            <div className="migration-title"><div><strong>完整环境迁移</strong><small>一次只迁移一个已停止的环境</small></div><span>本机原子覆盖 · 可回滚</span></div>
            <div className="form-grid">
              <div><label className="field-label" htmlFor="migration-source">MoreLogin 源环境</label><select id="migration-source" className="text-input" value={sourceProfileId} disabled={busy} onChange={(event) => selectSource(event.target.value)}>{preview.candidates.map((candidate) => <option key={candidate.sourceProfileId} value={candidate.sourceProfileId}>{candidate.name} · {candidate.sourceProfileId}</option>)}</select></div>
              <div><label className="field-label" htmlFor="migration-target">Brows 目标环境</label><select id="migration-target" className="text-input" value={targetProfileId} disabled={busy} onChange={(event) => { setTargetProfileId(event.target.value); setInspection(undefined); }}><option value="">请选择</option>{targets.filter((profile) => profile.status === "stopped").map((profile) => <option key={profile.id} value={profile.id}>{profile.name}</option>)}</select></div>
            </div>
            <button className="secondary-button" type="button" disabled={busy || !sourceProfileId || !targetProfileId} onClick={() => void inspectProfileData()}>{busy && !inspection ? "预检中…" : "完整迁移预检"}</button>
            {inspection && <div className="migration-inspection"><div><strong>{inspection.fileCount.toLocaleString()}</strong><span>文件</span></div><div><strong>{formatBytes(inspection.totalBytes)}</strong><span>数据量</span></div><div className="migration-capabilities">{[["历史", inspection.hasHistory], ["Cookie", inspection.hasCookies], ["登录状态", inspection.hasLoginData], ["LocalStorage", inspection.hasLocalStorage], ["IndexedDB", inspection.hasIndexedDb]].map(([label, present]) => <span className={present ? "present" : "missing"} key={String(label)}>{present ? "✓" : "—"} {label}</span>)}</div>{inspection.sourceMayBeRunning && <div className="form-error">检测到 MoreLogin 可能仍在使用此环境，请完全关闭后重试。</div>}</div>}
            {inspection && !migration && <button className="primary-button migration-execute" type="button" disabled={busy || inspection.sourceMayBeRunning} onClick={() => void executeMigration()}>{busy ? "正在复制并校验…" : "确认覆盖目标环境"}</button>}
            {migration?.status === "completed" && <div className="import-result success"><strong>完整迁移完成</strong><span>{migration.copiedFiles?.toLocaleString()} 个文件 · {formatBytes(migration.copiedBytes ?? 0)}</span><small>原目标数据已保留为本机备份，可从迁移记录回滚。</small></div>}
          </div>
        )}
        {history.length > 0 && <div className="migration-history"><strong>最近迁移记录</strong>{history.slice(0, 5).map((record) => <div className="migration-history-row" key={record.id}><span>#{record.id} · {record.sourceProfileId}</span><span>{migrationStatusLabel(record.status)}</span>{record.canRollback && <button className="table-button" type="button" disabled={busy} onClick={() => void rollback(record.id)}>回滚</button>}</div>)}</div>}
        {formError && <div className="form-error" role="alert">{formError}</div>}
        <div className="modal-actions">
          <button className="secondary-button" type="button" disabled={busy} onClick={onClose}>关闭</button>
          {!importResult && <button className="primary-button" type="button" disabled={!preview || busy} onClick={() => void importProfiles()}>{busy && preview ? "正在导入…" : `确认导入${preview ? ` ${preview.profileCount} 个` : ""}`}</button>}
        </div>
      </section>
    </div>
  );
}

function ProxyView({
  api,
  proxies,
  onRefresh,
  onError,
  onNotice,
}: {
  api: ApiClient;
  proxies: ProxySummary[];
  onRefresh(): Promise<void>;
  onError(message: string): void;
  onNotice(message: string): void;
}) {
  const [showForm, setShowForm] = useState(false);
  const [saving, setSaving] = useState(false);
  const [testingProxy, setTestingProxy] = useState<ProxySummary>();
  const [testing, setTesting] = useState(false);
  const [form, setForm] = useState({ name: "", kind: "http" as ProxyKind, host: "", port: "", username: "", password: "" });

  const save = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setSaving(true);
    try {
      await api.saveProxy({ name: form.name, kind: form.kind, host: form.host, port: Number(form.port), username: form.username || undefined, credential: form.password || undefined });
      setForm({ name: "", kind: "http", host: "", port: "", username: "", password: "" });
      setShowForm(false);
      await onRefresh();
      onNotice("代理已保存。密码只会进入系统凭据保险库。");
    } catch (cause) { onError(getErrorMessage(cause)); } finally { setSaving(false); }
  };

  const test = async (proxy: ProxySummary) => {
    setTesting(true);
    try {
      const checked = await api.testProxy(proxy.id);
      setTestingProxy(undefined);
      await onRefresh();
      onNotice(checked.lastExitIp ? `“${proxy.name}”认证和出口请求成功，出口 IP：${checked.lastExitIp}。` : `“${proxy.name}”认证及 HTTPS 隧道可用；明文 HTTP 出口 IP 端点不可用。`);
    } catch (cause) {
      const message = getErrorMessage(cause);
      setTestingProxy(undefined);
      await onRefresh();
      onError(message);
    } finally {
      setTesting(false);
    }
  };
  const remove = async (proxy: ProxySummary) => {
    try { await api.removeProxy(proxy.id); await onRefresh(); onNotice(`已删除代理“${proxy.name}”。`); } catch (cause) { onError(getErrorMessage(cause)); }
  };

  return (
    <section className="view-section">
      <div className="page-heading"><div><div className="eyebrow">NETWORK ROUTES</div><h1>代理库</h1><p>为每个环境绑定独立的 HTTP、HTTPS 或 SOCKS5 出口。代理凭据不会写入 SQLite。</p></div><button className="primary-button" type="button" onClick={() => setShowForm(true)}>＋ 添加代理</button></div>
      <div className="security-callout"><span aria-hidden="true">▣</span><span><strong>每个环境使用独立 loopback Broker</strong><small>HTTP CONNECT 和 SOCKS5 支持认证且失败不回退直连；TLS 加密的 HTTPS 上游代理暂未启用。</small></span></div>
      {showForm && <form className="inline-form" onSubmit={save}><div className="form-grid four"><div><label className="field-label" htmlFor="proxy-name">名称</label><input id="proxy-name" className="text-input" required value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="东京住宅代理" /></div><div><label className="field-label" htmlFor="proxy-kind">类型</label><select id="proxy-kind" className="text-input" value={form.kind} onChange={(e) => setForm({ ...form, kind: e.target.value as ProxyKind })}><option value="http">HTTP</option><option value="https">HTTPS</option><option value="socks5">SOCKS5</option></select></div><div><label className="field-label" htmlFor="proxy-host">主机</label><input id="proxy-host" className="text-input" required value={form.host} onChange={(e) => setForm({ ...form, host: e.target.value })} placeholder="proxy.example" /></div><div><label className="field-label" htmlFor="proxy-port">端口</label><input id="proxy-port" className="text-input" required type="number" min="1" max="65535" value={form.port} onChange={(e) => setForm({ ...form, port: e.target.value })} placeholder="8080" /></div></div><div className="form-grid"><div><label className="field-label" htmlFor="proxy-user">用户名（可选）</label><input id="proxy-user" className="text-input" value={form.username} onChange={(e) => setForm({ ...form, username: e.target.value })} /></div><div><label className="field-label" htmlFor="proxy-password">密码（可选）</label><input id="proxy-password" className="text-input" type="password" autoComplete="new-password" value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} /></div></div><div className="inline-form-actions"><button className="secondary-button" type="button" onClick={() => setShowForm(false)}>取消</button><button className="primary-button" disabled={saving} type="submit">{saving ? "保存中…" : "保存代理"}</button></div></form>}
      {proxies.length === 0 ? <div className="empty-state"><div className="empty-icon">⌁</div><strong>还没有代理</strong><span>添加代理后，可在创建环境时绑定。</span></div> : <div className="data-table-wrap"><table className="data-table"><thead><tr><th>名称</th><th>地址</th><th>类型</th><th>认证</th><th>状态</th><th aria-label="操作" /></tr></thead><tbody>{proxies.map((proxy) => <tr key={proxy.id}><td><strong>{proxy.name}</strong><small className="table-sub">{proxy.id.slice(0, 16)}</small></td><td className="mono">{proxy.host}{proxy.port ? `:${proxy.port}` : ""}</td><td><span className="type-badge">{proxy.kind.toUpperCase()}</span></td><td>{proxy.authenticated ? <span className="soft-success">已保护</span> : <span className="soft-muted">无认证</span>}</td><td><span className={`health-dot ${proxy.health}`} />{proxy.health === "healthy" ? `认证及出口可用${proxy.lastExitIp ? ` · ${proxy.lastExitIp}` : ""}` : proxy.health === "checking" ? "检查中" : proxy.health === "unhealthy" ? "认证或出口失败" : "未检查"}</td><td className="table-actions"><button className="table-button" type="button" onClick={() => setTestingProxy(proxy)}>测试</button><button className="table-button danger" type="button" onClick={() => void remove(proxy)}>删除</button></td></tr>)}</tbody></table></div>}
      {testingProxy && <div className="modal-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && !testing && setTestingProxy(undefined)}><section className="modal diagnostic-confirm-modal" role="dialog" aria-modal="true" aria-labelledby="proxy-test-title"><div className="modal-head"><div><div className="eyebrow">PROXY EXIT CHECK</div><h2 id="proxy-test-title">确认测试代理“{testingProxy.name}”</h2></div><button className="icon-button" type="button" disabled={testing} onClick={() => setTestingProxy(undefined)}>×</button></div><p className="modal-intro">将验证代理协议、用户名密码认证和真实出口，不再只检查 TCP 端口。</p><div className="diagnostic-consent"><strong>将通过该代理访问的唯一端点</strong><code>http://checkip.amazonaws.com/</code><ul><li>请求不包含 Cookie 或浏览器登录状态</li><li>凭据只从系统保险库进入临时 Broker，不写入日志或报告</li><li>认证或出口失败时不会回退本机直连</li></ul></div><div className="modal-actions"><button className="secondary-button" type="button" disabled={testing} onClick={() => setTestingProxy(undefined)}>取消</button><button className="primary-button" type="button" disabled={testing} onClick={() => void test(testingProxy)}>{testing ? "测试中…" : "确认并测试"}</button></div></section></div>}
    </section>
  );
}

function DiagnosticsView({ api, profiles, reports, onRefresh, onError, onNotice }: {
  api: ApiClient;
  profiles: Profile[];
  reports: DiagnosticReport[];
  onRefresh(): Promise<void>;
  onError(message: string): void;
  onNotice(message: string): void;
}) {
  const [selectedId, setSelectedId] = useState(profiles[0]?.id ?? "");
  const [running, setRunning] = useState(false);
  const [confirmExternal, setConfirmExternal] = useState(false);
  useEffect(() => {
    if (!profiles.some((profile) => profile.id === selectedId)) setSelectedId(profiles[0]?.id ?? "");
  }, [profiles, selectedId]);
  const report = reports.find((item) => item.profileId === selectedId);
  const run = async (external = false) => {
    if (!selectedId) return;
    setRunning(true);
    try {
      await api.runDiagnostic(selectedId, external);
      setConfirmExternal(false);
      await onRefresh();
      onNotice(external ? "主动网络检查完成。结果只保存在本机。" : "本地检查完成。结果只保存在本机。");
    } catch (cause) {
      onError(getErrorMessage(cause));
    } finally {
      setRunning(false);
    }
  };

  return <section className="view-section">
    <div className="page-heading">
      <div><div className="eyebrow">ENVIRONMENT DIAGNOSTICS</div><h1>指纹与泄漏诊断</h1><p>这里展示实际生效范围，不把代理 IP 或存储隔离夸大为完整设备指纹。</p></div>
      <div className="page-actions"><button className="secondary-button" type="button" disabled={!selectedId || running} onClick={() => void run(false)}>{running ? "检查中…" : "本地检查"}</button><button className="primary-button" type="button" disabled={!selectedId || running} onClick={() => setConfirmExternal(true)}>主动网络检查</button></div>
    </div>
    <div className="diagnostic-toolbar"><label className="field-label" htmlFor="diagnostic-profile">选择环境</label><select id="diagnostic-profile" className="text-input" value={selectedId} onChange={(event) => setSelectedId(event.target.value)}><option value="">请选择</option>{profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name}</option>)}</select><span className="diagnostic-scope-legend"><span className="scope-badge scope-isolated">独立隔离</span><span className="scope-badge scope-configurable">可配置</span><span className="scope-badge scope-shared">主机共享</span></span></div>
    {!report ? <div className="empty-state compact"><div className="empty-icon">⌁</div><strong>尚未运行诊断</strong><span>选择一个环境后开始检查。主动网络检查会在执行前显示端点。</span></div> : <div className="diagnostic-report">
      <div className={`report-banner ${report.overall}`}><span className="report-score">{report.overall === "pass" ? "✓" : "!"}</span><div><strong>{report.overall === "pass" ? "检查通过" : "存在需要关注的项目"}</strong><p>检查时间 {formatDate(report.checkedAt)} · {report.externalEndpoints.length === 0 ? "本次未访问外部检测端点" : `已访问 ${report.externalEndpoints.join("、")}`}</p></div></div>
      <div className="diagnostic-grid">{report.items.map((item) => <article className="diagnostic-item" key={item.key}><div className="diagnostic-item-head"><span className={`scope-badge ${scopeMeta[item.scope].className}`}>{scopeMeta[item.scope].label}</span><span className={`result-mark ${item.status}`}>{item.status === "pass" ? "✓" : item.status === "warning" ? "!" : "×"}</span></div><h3>{item.label}</h3><div className="diagnostic-values"><span>实际</span><strong>{item.actual}</strong>{item.expected && <><span>预期</span><strong>{item.expected}</strong></>}</div><p>{item.explanation}</p></article>)}</div>
    </div>}
    {confirmExternal && <div className="modal-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && !running && setConfirmExternal(false)}><section className="modal diagnostic-confirm-modal" role="dialog" aria-modal="true" aria-labelledby="diagnostic-confirm-title"><div className="modal-head"><div><div className="eyebrow">EXTERNAL NETWORK CHECK</div><h2 id="diagnostic-confirm-title">确认主动网络检查</h2></div><button className="icon-button" type="button" disabled={running} onClick={() => setConfirmExternal(false)}>×</button></div><p className="modal-intro">本次只检查出口 IP。执行前向你披露固定检测端点，DNS 与 WebRTC 本轮保持“未检查”。</p><div className="diagnostic-consent"><strong>将访问的唯一端点</strong><code>http://checkip.amazonaws.com/</code><ul><li>发送一次不含 Cookie 的 HTTP 请求</li><li>绑定代理时走该环境的临时 Broker；未绑定时使用本机直连</li><li>不启动环境，不读取登录状态、Cookie 或浏览历史</li></ul></div><div className="modal-actions"><button className="secondary-button" type="button" disabled={running} onClick={() => setConfirmExternal(false)}>取消</button><button className="primary-button" type="button" disabled={running} onClick={() => void run(true)}>{running ? "检查中…" : "确认并检查"}</button></div></section></div>}
  </section>;
}

const backupPhaseLabels: Record<BackupPhase, string> = {
  preparing: "检查文件", deriving_key: "派生加密密钥", processing: "处理归档数据",
  verifying: "校验并同步", committing: "提交结果", cleaning: "清理暂存",
  completed: "已完成", cancelled: "已取消", failed: "失败",
};

function backupBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function BackupsView({ api, profiles, backups, onRefresh, onError, onNotice }: {
  api: ApiClient;
  profiles: Profile[];
  backups: BackupJob[];
  onRefresh(): Promise<void>;
  onError(message: string): void;
  onNotice(message: string): void;
}) {
  const [profileId, setProfileId] = useState(profiles[0]?.id ?? "");
  const [destinationName, setDestinationName] = useState("profile-backup.browsbak");
  const [password, setPassword] = useState("");
  const [passwordConfirm, setPasswordConfirm] = useState("");
  const [importName, setImportName] = useState("");
  const [importPassword, setImportPassword] = useState("");
  const [busy, setBusy] = useState<"export" | "import" | "cleanup" | undefined>();
  const [cancelling, setCancelling] = useState<string | undefined>();
  const occupied = busy !== undefined || backups.some((job) => job.status === "running");
  useEffect(() => {
    if (!profiles.some((profile) => profile.id === profileId)) setProfileId(profiles[0]?.id ?? "");
  }, [profiles, profileId]);

  const handleFailure = (cause: unknown) => {
    if (cause instanceof ApiError && cause.code === "OPERATION_CANCELLED") {
      onNotice("任务已取消；清理结果可在任务记录中查看。");
    } else onError(getErrorMessage(cause));
  };
  const cleanup = async () => {
    setBusy("cleanup");
    try {
      const result = await api.cleanupBackupStaging();
      await onRefresh();
      onNotice(result.cleanupPending ? "部分暂存无法安全清理，已保留，可稍后重试。" : "暂存检查完成；已有环境数据未修改。");
    } catch (cause) { onError(getErrorMessage(cause)); }
    finally { setBusy(undefined); }
  };
  const exportBackup = async () => {
    if (!profileId || occupied) return;
    if (password.length < 12) { onError("恢复口令至少需要 12 个字符"); return; }
    if (password !== passwordConfirm) { onError("两次输入的恢复口令不一致"); return; }
    if (!destinationName.toLowerCase().endsWith(".browsbak")) { onError("文件名必须以 .browsbak 结尾"); return; }
    setBusy("export");
    const operation = api.exportBackup(profileId, destinationName, false, password);
    setPassword(""); setPasswordConfirm("");
    try { await operation; await onRefresh(); onNotice("加密备份已创建"); }
    catch (cause) { await onRefresh(); handleFailure(cause); }
    finally { setBusy(undefined); }
  };
  const importBackup = async () => {
    if (occupied) return;
    if (!importName || importPassword.length < 12) { onError("请输入归档文件名和至少 12 个字符的恢复口令"); return; }
    setBusy("import");
    const operation = api.importBackup(importName, importPassword);
    setImportPassword("");
    try { const profile = await operation; await onRefresh(); onNotice(`已恢复环境：${profile.name}`); }
    catch (cause) { await onRefresh(); handleFailure(cause); }
    finally { setBusy(undefined); }
  };
  const cancel = async (operationId: string) => {
    setCancelling(operationId);
    try { await api.cancelBackup(operationId); await onRefresh(); onNotice("取消请求已接受，处理和清理结果会显示在任务记录中。"); }
    catch (cause) { await onRefresh(); onError(getErrorMessage(cause)); }
    finally { setCancelling(undefined); }
  };

  return <section className="view-section">
    <div className="page-heading"><div><div className="eyebrow">ENCRYPTED ARCHIVES</div><h1>备份与恢复</h1><p>使用恢复口令加密完整 Chromium Profile。恢复会生成新的环境 UUID，不覆盖现有环境。</p></div></div>
    <div className="backup-panel">
      <div className="backup-panel-head"><div><h2>导出完整环境</h2><p>包含 Cookie、缓存和站点存储；代理密码不会写入归档。</p></div><span className="vault-badge">Argon2id · XChaCha20</span></div>
      <div className="form-grid">
        <div><label className="field-label" htmlFor="backup-profile">选择环境</label><select id="backup-profile" className="text-input" value={profileId} onChange={(e) => setProfileId(e.target.value)}><option value="">请选择</option>{profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.name}</option>)}</select></div>
        <div><label className="field-label" htmlFor="backup-name">备份文件名</label><input id="backup-name" className="text-input" value={destinationName} onChange={(e) => setDestinationName(e.target.value)} /></div>
        <div><label className="field-label" htmlFor="backup-password">恢复口令（至少 12 个字符）</label><input id="backup-password" className="text-input" type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} /></div>
        <div><label className="field-label" htmlFor="backup-password-confirm">确认恢复口令</label><input id="backup-password-confirm" className="text-input" type="password" autoComplete="new-password" value={passwordConfirm} onChange={(e) => setPasswordConfirm(e.target.value)} /></div>
      </div>
      <div className="backup-warning"><span aria-hidden="true">i</span> Chromium 的 DPAPI / App-Bound Encryption 可能让登录态无法跨 Windows 设备直接使用。备份恢复配置成功不代表免登录。</div>
      <button className="primary-button" type="button" disabled={occupied || !profileId || !password || !passwordConfirm} onClick={() => void exportBackup()}>{busy === "export" ? "正在导出…" : "导出加密备份"}</button>
    </div>
    <div className="backup-panel">
      <div className="backup-panel-head"><div><h2>恢复加密归档</h2><p>输入应用备份目录中的文件名；恢复将创建新的环境并安装隔离 userdata。</p></div></div>
      <div className="form-grid">
        <div><label className="field-label" htmlFor="restore-name">归档文件名</label><input id="restore-name" className="text-input" placeholder="profile-backup.browsbak" value={importName} onChange={(e) => setImportName(e.target.value)} /></div>
        <div><label className="field-label" htmlFor="restore-password">恢复口令</label><input id="restore-password" className="text-input" type="password" autoComplete="current-password" value={importPassword} onChange={(e) => setImportPassword(e.target.value)} /></div>
      </div>
      <button className="secondary-button" type="button" disabled={occupied || !importName || !importPassword} onClick={() => void importBackup()}>{busy === "import" ? "正在恢复…" : "恢复为新环境"}</button>
    </div>
    <div className="subsection-heading"><div><h2>归档与恢复记录</h2><span>恢复结果跨重启保留；每次处理一个任务</span></div><button className="secondary-button" type="button" disabled={occupied} onClick={() => void cleanup()}>{busy === "cleanup" ? "清理中…" : "重试暂存清理"}</button></div>
    {backups.length === 0 ? <div className="empty-state compact"><div className="empty-icon">↥</div><strong>还没有备份任务</strong><span>完成的归档会显示在这里。</span></div> : <div className="data-table-wrap"><table className="data-table">
      <thead><tr><th>环境</th><th>状态</th><th>创建时间</th><th>进度与备注</th><th>操作</th></tr></thead>
      <tbody>{backups.map((backup) => <tr key={backup.id}>
        <td><strong>{backup.profileName}</strong></td>
        <td><span className={`status-pill ${backup.status === "completed" ? "green" : (backup.status === "failed" || backup.status === "interrupted") ? "red" : "blue"}`}>{{ ready: "待处理", running: "进行中", completed: "已完成", failed: "失败", interrupted: "已中断", cancelled: "已取消" }[backup.status]}</span></td>
        <td>{formatDate(backup.createdAt)}</td>
        <td>
          {backup.phase && <div>{backupPhaseLabels[backup.phase]}{backup.status === "running" && backup.cancelRequested ? " · 已请求取消，等待收尾" : ""}</div>}
          {backup.processedBytes !== undefined && <div>已处理归档数据 {backupBytes(backup.processedBytes)}{backup.totalBytes !== undefined ? ` / ${backupBytes(backup.totalBytes)}` : ""}</div>}
          {backup.phase === "deriving_key" && <div>取消需等待当前密钥计算结束。</div>}
          {backup.status === "running" && (backup.phase === "committing" || backup.phase === "cleaning") && <div>正在收尾，请等待完成。</div>}
          {backup.note ?? (!backup.phase ? "—" : "")}
        </td>
        <td>{backup.operationId && backup.status === "running" && <button className="secondary-button" type="button" disabled={!backup.canCancel || backup.cancelRequested || cancelling === backup.operationId} onClick={() => void cancel(backup.operationId!)}>{backup.cancelRequested || cancelling === backup.operationId ? "等待取消…" : "取消任务"}</button>}</td>
      </tr>)}</tbody>
    </table></div>}
  </section>;
}

function fingerprintSupportLabel(runtime?: BrowserRuntime): string {
  switch (runtime?.fingerprintSupport) {
    case "supported": return "指纹加载检测通过";
    case "unsupported": return "指纹加载检测未通过";
    default: return "指纹支持未验证";
  }
}

function RuntimeFingerprintRow({ api, runtime, onRefresh, onError, onNotice }: {
  api: ApiClient;
  runtime: BrowserRuntime;
  onRefresh(): Promise<void>;
  onError(message: string): void;
  onNotice(message: string): void;
}) {
  const [probing, setProbing] = useState(false);
  const [detail, setDetail] = useState("");
  const [probeError, setProbeError] = useState("");
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const probe = async () => {
    if (probing || !runtime.trusted) return;
    setProbing(true);
    setDetail("");
    setProbeError("");
    try {
      const result = await api.probeRuntimeFingerprint(runtime.id);
      if (mounted.current) setDetail(result.detail);
      await onRefresh();
      if (mounted.current) onNotice(`${runtime.name}：${fingerprintSupportLabel(result.runtime)}。探针结果不代表完整 anti-detect。`);
    } catch (cause) {
      const message = getErrorMessage(cause);
      if (mounted.current) {
        setProbeError(message);
        onError(message);
      }
      // A failed re-probe clears prior backend readiness. Refresh even on
      // transport/timeout failure so an old "supported" badge cannot linger.
      await onRefresh();
    } finally {
      if (mounted.current) setProbing(false);
    }
  };

  return (
    <div>
      <div className="runtime-row">
        <span className="runtime-logo" aria-hidden="true">C</span>
        <div><strong>{runtime.name}</strong><small>{runtime.version} · {runtime.channel}</small><small>{fingerprintSupportLabel(runtime)}</small></div>
        <span className={`runtime-trust ${runtime.trusted ? "trusted" : "untrusted"}`}>{runtime.trusted ? "签名已校验" : "未登记"}</span>
      </div>
      {runtime.trusted && <button className="secondary-button full" type="button" disabled={probing} aria-busy={probing} onClick={() => void probe()}>{probing ? "正在检测指纹加载…" : `检测 ${runtime.name} 指纹支持`}</button>}
      {probing && <div className="form-info" role="status">正在启动独立临时浏览器进行本地探针检测，不使用已保存环境或访问外部检测站。</div>}
      {detail && <div className="form-info" role="status">{detail}</div>}
      {probeError && <div className="form-error" role="alert">{probeError}</div>}
    </div>
  );
}

function SettingsView({
  api,
  settings,
  runtimes,
  onRefresh,
  onError,
  onNotice,
}: {
  api: ApiClient;
  settings?: Settings;
  runtimes: BrowserRuntime[];
  onRefresh(): Promise<void>;
  onError(message: string): void;
  onNotice(message: string): void;
}) {
  const [saving, setSaving] = useState(false);
  if (!settings) return <LoadingState />;
  const save = async (changes: Partial<Settings>) => {
    setSaving(true);
    try { await api.updateSettings(changes); await onRefresh(); onNotice("设置已保存。"); } catch (cause) { onError(getErrorMessage(cause)); } finally { setSaving(false); }
  };
  return (
    <section className="view-section">
      <div className="page-heading"><div><div className="eyebrow">LOCAL CONTROL PLANE</div><h1>设置</h1><p>管理本地运行时、并发资源和安全默认值。</p></div></div>
      <div className="settings-grid">
        <article className="settings-card">
          <div className="settings-card-head"><div><h2>会话调度</h2><p>同一环境只允许一个活动会话。</p></div><span className="settings-icon">↯</span></div>
          <label className="field-label" htmlFor="max-concurrent">最大并发环境</label>
          <div className="number-control"><select id="max-concurrent" className="text-input" value={settings.maxConcurrentSessions} disabled={saving} onChange={(e) => void save({ maxConcurrentSessions: Number(e.target.value) })}>{Array.from({ length: 10 }, (_, index) => index + 1).map((limit) => <option key={limit} value={limit}>{limit}</option>)}</select><span>个</span></div>
          <p className="form-info">保存后立即生效并在重启后保留。降低上限不会停止现有会话；提高上限会按排队顺序启动。</p>
          <label className="switch-row"><input type="checkbox" checked={settings.stopSessionsOnExit} disabled /><span className="switch-ui" /><span><strong>退出管理器时停止会话</strong><small>固定开启，确保浏览器和代理资源随管理器退出回收。</small></span></label>
        </article>
        <article className="settings-card">
          <div className="settings-card-head"><div><h2>浏览器运行时</h2><p>签名可信 ≠ 指纹支持；指纹加载检测通过 ≠ 完整 anti-detect。</p></div><span className="settings-icon">◉</span></div>
          {runtimes.length === 0 && <div className="form-info">没有可信浏览器运行时。请安装或修复官方 Google Chrome，然后重新启动管理器；已有环境仍可查看和备份。</div>}
          {runtimes.map((runtime) => <RuntimeFingerprintRow key={`${runtime.id}:${runtime.version}`} api={api} runtime={runtime} onRefresh={onRefresh} onError={onError} onNotice={onNotice} />)}
          <div className="form-info">检测会创建独立临时环境，仅验证本地扩展注入。SeedableNoise 启动前必须通过；系统原生不受此限制。</div>
          <button className="text-button" type="button" onClick={() => onNotice("启动时验证 Windows Authenticode 信任链与 Google LLC 发行者，并在每次浏览器启动前重新核对 SHA-256。")}>查看运行时信任说明 →</button>
        </article>
        <article className="settings-card">
          <div className="settings-card-head"><div><h2>隐私与审计</h2><p>默认不向任何远程服务发送数据。</p></div><span className="settings-icon">⌁</span></div>
          <div className="privacy-row"><span className="privacy-check">✓</span><div><strong>遥测已关闭</strong><small>此设置由产品安全基线锁定。</small></div><span className="locked-label">锁定</span></div>
          <div className="privacy-row"><span className="privacy-check">✓</span><div><strong>日志自动脱敏</strong><small>不记录 Cookie、Token、密码或完整代理 URL。</small></div><span className="locked-label">默认</span></div>
          <button className="text-button" type="button" onClick={() => onNotice("Profile 与会话操作的脱敏审计记录保存在本机 SQLite；不记录 Cookie、Token、密码或代理 URL。")}>查看安全说明 →</button>
        </article>
      </div>
    </section>
  );
}

export default App;
