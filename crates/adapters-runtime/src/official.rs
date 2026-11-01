//! Fixed-source Chrome for Testing installation. No user-supplied URL, path,
//! browser flags or publisher allowlist crosses the IPC boundary.
use super::*;
use brows_domain::BrowserCapabilities;
use serde::{Deserialize, Serialize};
use std::{io::Write, sync::Condvar};
mod http;
#[cfg(test)]
mod tests;

pub(super) const CATALOG: &str = "https://raw.githubusercontent.com/GoogleChromeLabs/chrome-for-testing/main/data/last-known-good-versions-with-downloads.json";
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
const MAX_RECEIPT: u64 = 2 * 1024 * 1024;
const PROVIDER: &str = "Chrome for Testing";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInstallStatus {
    pub available: bool,
    pub running: bool,
    pub phase: &'static str,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub can_cancel: bool,
    pub cancel_requested: bool,
    pub detail: String,
}
impl Default for RuntimeInstallStatus {
    fn default() -> Self {
        Self {
            available: cfg!(windows),
            running: false,
            phase: "idle",
            downloaded_bytes: 0,
            total_bytes: None,
            can_cancel: false,
            cancel_requested: false,
            detail: String::new(),
        }
    }
}
#[derive(Debug, Default)]
struct State {
    stopping: bool,
    status: RuntimeInstallStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    version: String,
    archive_sha256: String,
    installed_at_ms: u64,
    files: Vec<bundle::FileHash>,
}
impl Receipt {
    fn id(&self) -> String {
        format!("cft-{}-{}", self.version, self.archive_sha256)
    }
    fn validate(&self) -> PortResult<()> {
        if self.schema != 1
            || self.archive_sha256.len() != 64
            || !self
                .archive_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.files.is_empty()
            || self.files.len() > bundle::MAX_FILES
        {
            return Err(invalid_data("invalid managed runtime receipt"));
        }
        parse_version(&self.version)?;
        let mut paths = HashSet::new();
        let mut total = 0u64;
        for file in &self.files {
            bundle::safe_relative(&file.path)?;
            total = total
                .checked_add(file.bytes)
                .ok_or_else(|| invalid_data("runtime receipt size overflow"))?;
            if !paths.insert(file.path.to_ascii_lowercase())
                || file.sha256.len() != 64
                || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                || total > bundle::MAX_BYTES
            {
                return Err(invalid_data("invalid managed runtime file receipt"));
            }
        }
        Ok(())
    }
}

trait Source {
    fn fetch(
        &self,
        url: &str,
        output: &mut dyn Write,
        limit: u64,
        control: &OfficialRuntimeInstaller,
    ) -> PortResult<String>;
}
trait Verifier {
    fn verify(&self, executable: &Path, version: &str) -> PortResult<String>;
}
struct NativeVerifier;
impl Verifier for NativeVerifier {
    fn verify(&self, executable: &Path, expected: &str) -> PortResult<String> {
        let publisher = verify_authenticode_publisher(executable, &["Google LLC"])?;
        if version::file_version(executable)? != parse_version(expected)? {
            return Err(bundle::untrusted("浏览器实际版本与官方目录不一致"));
        }
        Ok(publisher)
    }
}

/// Owns installation admission and worker shutdown independently of UI futures.
#[derive(Debug)]
pub struct OfficialRuntimeInstaller {
    root: PathBuf,
    runtime: Arc<ManagedChromiumRuntime>,
    state: Mutex<State>,
    finished: Condvar,
}
impl OfficialRuntimeInstaller {
    pub fn new(root: impl Into<PathBuf>, runtime: Arc<ManagedChromiumRuntime>) -> PortResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|_| internal("无法创建浏览器安装目录"))?;
        bundle::regular_directory(&root)?;
        let root = root
            .canonicalize()
            .map_err(|_| invalid_data("无法解析浏览器安装目录"))?;
        if !runtime
            .runtime_roots
            .iter()
            .any(|allowed| root.starts_with(allowed))
        {
            return Err(invalid_data(
                "installer root is not a controlled runtime root",
            ));
        }
        Ok(Self {
            root,
            runtime,
            state: Mutex::new(State::default()),
            finished: Condvar::new(),
        })
    }
    pub fn status(&self) -> RuntimeInstallStatus {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn cancel(&self) -> PortResult<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status.running && state.status.can_cancel {
            state.status.cancel_requested = true;
            Ok(())
        } else {
            Err(PortError::new(
                PortErrorKind::InvalidData,
                "安装已结束或进入提交、探针检测阶段，不能取消",
            ))
        }
    }
    pub fn stop_accepting(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stopping = true;
        if state.status.running && state.status.can_cancel {
            state.status.cancel_requested = true;
        }
    }
    pub fn shutdown(&self) {
        self.stop_accepting();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.finished
                .wait_while(state, |state| state.status.running)
                .unwrap_or_else(|e| e.into_inner()),
        );
    }
    fn begin(&self) -> PortResult<Lease<'_>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping {
            return Err(PortError::new(PortErrorKind::Unavailable, "管理器正在退出"));
        }
        if state.status.running {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "已有浏览器安装任务正在执行",
            ));
        }
        state.status = RuntimeInstallStatus {
            running: true,
            phase: "catalog",
            can_cancel: true,
            ..Default::default()
        };
        Ok(Lease(self))
    }
    fn checkpoint(&self) -> PortResult<()> {
        if self.status().cancel_requested {
            Err(PortError::new(PortErrorKind::Cancelled, "浏览器安装已取消"))
        } else {
            Ok(())
        }
    }
    fn phase(&self, phase: &'static str) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .phase = phase;
    }
    fn reset_bytes(&self, total: Option<u64>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status.downloaded_bytes = 0;
        state.status.total_bytes = total;
    }
    fn bytes(&self, n: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status.downloaded_bytes = state.status.downloaded_bytes.saturating_add(n as u64);
    }
    fn commit(&self) -> PortResult<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.status.cancel_requested {
            return Err(PortError::new(PortErrorKind::Cancelled, "浏览器安装已取消"));
        }
        state.status.can_cancel = false;
        state.status.phase = "committing";
        Ok(())
    }
    pub fn install_stable(&self) -> PortResult<FingerprintProbeResult> {
        if !cfg!(windows) {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "官方浏览器安装仅在 Windows 桌面版可用",
            ));
        }
        self.install_with(&http::HttpSource, &NativeVerifier)
    }
    fn install_with(
        &self,
        source: &dyn Source,
        verifier: &dyn Verifier,
    ) -> PortResult<FingerprintProbeResult> {
        let _lease = self.begin()?;
        let result = self.install_inner(source, verifier);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.status.can_cancel = false;
        match &result {
            Ok(result) => {
                state.status.phase = if result.build.capabilities.fingerprint_support
                    == FingerprintSupport::Supported
                {
                    "ready"
                } else {
                    "unsupported"
                };
                state.status.detail = result.detail.clone();
            }
            Err(error) => {
                state.status.phase = if error.kind == PortErrorKind::Cancelled {
                    "cancelled"
                } else {
                    "failed"
                };
                state.status.detail = error.message.clone();
            }
        }
        result
    }
    fn install_inner(
        &self,
        source: &dyn Source,
        verifier: &dyn Verifier,
    ) -> PortResult<FingerprintProbeResult> {
        let mut metadata = Vec::new();
        source.fetch(CATALOG, &mut metadata, 4 * 1024 * 1024, self)?;
        self.checkpoint()?;
        let (version, url) = catalog(&metadata)?;
        let installed = self.runtime.installed_builds()?;
        for build in installed.iter().filter(|build| build.provider == PROVIDER) {
            if parse_version(&version)? < parse_version(&build.version)? {
                return Err(bundle::untrusted("官方目录版本低于已安装版本，已拒绝降级"));
            }
        }
        if let Some(build) = installed
            .iter()
            .find(|build| build.provider == PROVIDER && build.version == version)
        {
            self.runtime.verify_build_integrity(build)?;
            self.commit()?;
            return self.probe(build);
        }
        let mut entries = 0usize;
        for entry in fs::read_dir(&self.root)
            .map_err(|_| internal("无法检查运行时目录容量"))?
            .take(64)
        {
            entry.map_err(|_| invalid_data("无法检查运行时目录条目"))?;
            entries += 1;
        }
        if entries >= 64 {
            return Err(PortError::new(
                PortErrorKind::Unavailable,
                "运行时目录已达到条目上限，请先检查并清理不再使用的版本或暂存",
            ));
        }
        let staging = self.allocate_staging()?;
        let attempt = (|| {
            self.phase("downloading");
            self.reset_bytes(None);
            let mut archive = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(staging.join("download.zip"))
                .map_err(|_| internal("无法创建浏览器下载文件"))?;
            let archive_sha256 = source.fetch(&url, &mut archive, MAX_DOWNLOAD, self)?;
            archive
                .sync_all()
                .map_err(|_| internal("无法同步浏览器安装包"))?;
            drop(archive);
            self.checkpoint()?;
            self.phase("extracting");
            let package = staging.join("package");
            let payload = package.join("chrome-win64");
            fs::create_dir(&package).map_err(|_| internal("无法创建运行时包目录"))?;
            fs::create_dir(&payload).map_err(|_| internal("无法创建运行时解包目录"))?;
            let files = bundle::extract(
                File::open(staging.join("download.zip"))
                    .map_err(|_| invalid_data("无法读取安装包"))?,
                &payload,
                &|| self.checkpoint(),
            )?;
            self.phase("verifying");
            self.checkpoint()?;
            verifier.verify(&payload.join("chrome.exe"), &version)?;
            self.checkpoint()?;
            let receipt = Receipt {
                schema: 1,
                version,
                archive_sha256,
                installed_at_ms: now_ms(),
                files,
            };
            receipt.validate()?;
            let data =
                serde_json::to_vec(&receipt).map_err(|_| internal("无法序列化运行时收据"))?;
            if data.len() as u64 > MAX_RECEIPT {
                return Err(invalid_data("runtime receipt too large"));
            }
            let mut output = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(package.join("receipt.json"))
                .map_err(|_| internal("无法创建运行时收据"))?;
            output
                .write_all(&data)
                .and_then(|()| output.sync_all())
                .map_err(|_| internal("无法保存运行时收据"))?;
            drop(output);
            let destination = self.root.join(receipt.id());
            if fs::symlink_metadata(&destination).is_ok() {
                return Err(PortError::new(
                    PortErrorKind::AlreadyExists,
                    "相同安装包已存在，请检查已安装运行时",
                ));
            }
            self.commit()?;
            fs::rename(&package, &destination).map_err(|_| internal("无法发布浏览器运行时"))?;
            let build = self.admit(&destination, &receipt, verifier)?;
            self.probe(&build)
        })();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.status.can_cancel = false;
            state.status.phase = "cleaning";
        }
        // A committed package remains installed even if probing fails. Only the
        // exact operation's staging directory is eligible for cleanup.
        if self.remove_staging(&staging).is_err() {
            return match attempt {
                Ok(mut result) => {
                    result.detail.push_str("；下载暂存清理待处理，重启时会重试");
                    Ok(result)
                }
                Err(mut error) => {
                    error.message.push_str("；下载暂存清理待处理，重启时会重试");
                    Err(error)
                }
            };
        }
        attempt
    }
    fn probe(&self, build: &BrowserBuild) -> PortResult<FingerprintProbeResult> {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopping {
                return Ok(FingerprintProbeResult {
                    build: {
                        let mut pending = build.clone();
                        pending.capabilities.fingerprint_support = FingerprintSupport::Unverified;
                        pending
                    },
                    detail: "运行时已安装；管理器正在退出，可在下次启动后检测指纹能力".into(),
                });
            }
            state.status.phase = "probing";
        }
        match self.runtime.probe_fingerprint_support(&build.id) {
            Ok(result) => Ok(result),
            Err(error) if error.kind == PortErrorKind::RuntimeUntrusted => Err(error),
            Err(error) => {
                let current = self
                    .runtime
                    .resolve_build(&BrowserBuildSelector::Pinned(build.id.clone()))?;
                if current.capabilities.fingerprint_support == FingerprintSupport::Supported {
                    return Err(error);
                }
                Ok(FingerprintProbeResult {
                    build: current,
                    detail: format!("运行时已安装，但本地指纹加载检测未通过：{}", error.message),
                })
            }
        }
    }
    fn admit(
        &self,
        directory: &Path,
        receipt: &Receipt,
        verifier: &dyn Verifier,
    ) -> PortResult<BrowserBuild> {
        receipt.validate()?;
        bundle::regular_directory(directory)?;
        if directory.parent() != Some(self.root.as_path())
            || directory.file_name().and_then(|n| n.to_str()) != Some(receipt.id().as_str())
        {
            return Err(invalid_data("invalid managed runtime identity"));
        }
        let integrity = bundle::BundleIntegrity {
            root: directory.join("chrome-win64"),
            files: receipt.files.clone(),
        };
        integrity.verify()?;
        let executable = integrity.root.join("chrome.exe");
        let publisher = verifier.verify(&executable, &receipt.version)?;
        let digest = receipt
            .files
            .iter()
            .find(|file| file.path == "chrome.exe")
            .ok_or_else(|| invalid_data("missing browser executable receipt"))?
            .sha256
            .clone();
        let build = BrowserBuild {
            id: BrowserBuildId::new(receipt.id()).map_err(|e| invalid_data(e.to_string()))?,
            provider: PROVIDER.into(),
            version: receipt.version.clone(),
            channel: BrowserChannel::Stable,
            executable_path: executable,
            sha256: digest,
            publisher,
            trust: RuntimeTrust::Verified,
            capabilities: BrowserCapabilities {
                supports_language: true,
                supports_proxy: true,
                supports_webrtc_policy: true,
                supports_devtools_pipe: true,
                fingerprint_support: FingerprintSupport::Unverified,
            },
            installed_at_ms: receipt.installed_at_ms,
        };
        let activate = match self
            .runtime
            .resolve_build(&BrowserBuildSelector::Channel(BrowserChannel::Stable))
        {
            Ok(current) if current.provider == PROVIDER => {
                parse_version(&build.version)? > parse_version(&current.version)?
            }
            Ok(_) => false,
            Err(_) => true,
        };
        self.runtime
            .register_build_checked(build.clone(), activate, Some(integrity))?;
        Ok(build)
    }
    /// Re-admits immutable packages after full integrity, publisher and version
    /// checks. Readiness is intentionally reset to Unverified on each app run.
    pub fn load_installed(&self) -> PortResult<Vec<String>> {
        let result = self.load_with(&NativeVerifier);
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .detail = match &result {
            Ok(warnings) => warnings.join("；"),
            Err(_) => "已安装运行时目录无法完整验证；请检查安装目录".into(),
        };
        result
    }
    fn load_with(&self, verifier: &dyn Verifier) -> PortResult<Vec<String>> {
        let mut warnings = Vec::new();
        let mut count = 0usize;
        for entry in fs::read_dir(&self.root).map_err(|_| internal("无法读取运行时安装目录"))?
        {
            let entry = entry.map_err(|_| invalid_data("无法读取运行时条目"))?;
            count += 1;
            if count > 64 {
                return Err(invalid_data("运行时目录条目过多，请检查安装目录"));
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".install-") {
                if self.remove_staging(&path).is_err() {
                    warnings.push("浏览器下载暂存未能安全清理，已保留".into());
                }
                continue;
            }
            let result = (|| {
                bundle::regular_directory(&path)?;
                let receipt_path = path.join("receipt.json");
                let metadata = fs::symlink_metadata(&receipt_path)
                    .map_err(|_| invalid_data("运行时收据缺失"))?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || is_reparse_point(&metadata)
                    || metadata.len() > MAX_RECEIPT
                {
                    return Err(invalid_data("invalid runtime receipt file"));
                }
                let data =
                    fs::read(receipt_path).map_err(|_| invalid_data("无法读取运行时收据"))?;
                let receipt: Receipt = serde_json::from_slice(&data)
                    .map_err(|_| invalid_data("运行时收据格式无效"))?;
                self.admit(&path, &receipt, verifier)
            })();
            if result.is_err() {
                warnings.push("某个已安装运行时未通过验证，已保留文件并禁止使用".into());
            }
        }
        Ok(warnings)
    }
    fn allocate_staging(&self) -> PortResult<PathBuf> {
        for _ in 0..16 {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| internal("无法分配运行时安装任务"))?;
            let path = self
                .root
                .join(format!(".install-{:032x}", u128::from_le_bytes(random)));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(internal("无法创建运行时暂存目录")),
            }
        }
        Err(internal("无法分配唯一运行时暂存目录"))
    }
    fn remove_staging(&self, path: &Path) -> PortResult<()> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let key = name.strip_prefix(".install-").unwrap_or_default();
        if path.parent() != Some(self.root.as_path())
            || key.len() != 32
            || !key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid_data("unrecognized runtime staging"));
        }
        bundle::regular_directory(path)?;
        // Scan before removal; links and unknown top-level names are preserved.
        for entry in
            fs::read_dir(path).map_err(|_| invalid_data("cannot inspect runtime staging"))?
        {
            let entry = entry.map_err(|_| invalid_data("cannot inspect runtime staging entry"))?;
            let name = entry.file_name();
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| invalid_data("cannot inspect runtime staging type"))?;
            if name == "download.zip"
                && metadata.is_file()
                && !metadata.file_type().is_symlink()
                && !is_reparse_point(&metadata)
            {
                continue;
            }
            if name == "package" && metadata.is_dir() {
                bundle::regular_directory(&entry.path())?;
                for child in fs::read_dir(entry.path())
                    .map_err(|_| invalid_data("cannot inspect runtime staging package"))?
                {
                    let child = child.map_err(|_| {
                        invalid_data("cannot inspect runtime staging package entry")
                    })?;
                    let metadata = fs::symlink_metadata(child.path())
                        .map_err(|_| invalid_data("cannot inspect runtime staging package type"))?;
                    if child.file_name() == "chrome-win64" && metadata.is_dir() {
                        let _ = bundle::scan(&child.path(), &|| Ok(()))?;
                    } else if child.file_name() != "receipt.json"
                        || !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || is_reparse_point(&metadata)
                        || metadata.len() > MAX_RECEIPT
                    {
                        return Err(invalid_data("unexpected runtime staging package content"));
                    }
                }
                continue;
            }
            return Err(invalid_data("unexpected runtime staging content"));
        }
        fs::remove_dir_all(path).map_err(|_| internal("无法清理浏览器下载暂存"))
    }
}
struct Lease<'a>(&'a OfficialRuntimeInstaller);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            state.status.phase,
            "ready" | "unsupported" | "failed" | "cancelled"
        ) {
            state.status.phase = "failed";
        }
        state.status.can_cancel = false;
        state.status.running = false;
        self.0.finished.notify_all();
    }
}

fn parse_version(version: &str) -> PortResult<[u32; 4]> {
    let parts = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_data("invalid official browser version"))?;
    let parts: [u32; 4] = parts
        .try_into()
        .map_err(|_| invalid_data("expected four version components"))?;
    if parts[0] == 0
        || parts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(".")
            != version
    {
        return Err(invalid_data("non-canonical official browser version"));
    }
    Ok(parts)
}
fn archive_url(version: &str) -> PortResult<String> {
    parse_version(version)?;
    Ok(format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{version}/win64/chrome-win64.zip"
    ))
}
fn allowed_url(url: &str) -> bool {
    url == CATALOG
        || url
            .strip_prefix("https://storage.googleapis.com/chrome-for-testing-public/")
            .and_then(|tail| tail.strip_suffix("/win64/chrome-win64.zip"))
            .is_some_and(|v| archive_url(v).is_ok_and(|expected| expected == url))
}
fn catalog(data: &[u8]) -> PortResult<(String, String)> {
    let value: Value =
        serde_json::from_slice(data).map_err(|_| invalid_data("官方浏览器目录格式无效"))?;
    let stable = &value["channels"]["Stable"];
    if stable["channel"].as_str() != Some("Stable") {
        return Err(invalid_data("官方目录未提供稳定版"));
    }
    let version = stable["version"]
        .as_str()
        .ok_or_else(|| invalid_data("官方目录缺少版本"))?;
    let expected = archive_url(version)?;
    let downloads = stable["downloads"]["chrome"]
        .as_array()
        .ok_or_else(|| invalid_data("官方目录缺少浏览器下载项"))?;
    let windows = downloads
        .iter()
        .filter(|item| item["platform"].as_str() == Some("win64"))
        .collect::<Vec<_>>();
    if windows.len() != 1 || windows[0]["url"].as_str() != Some(expected.as_str()) {
        return Err(bundle::untrusted("官方目录下载地址与固定来源不一致"));
    }
    Ok((version.into(), expected))
}
