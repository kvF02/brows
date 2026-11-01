#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use brows_application::{
    LegacyMigrationArtifact, LegacyProfileDataMigrator, LegacyProfileDataSummary, PortError,
    PortErrorKind, PortResult,
};
use brows_domain::ProfileId;
use calamine::{open_workbook_auto, Reader};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

mod recovery;

const MAX_EXPORT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MIGRATION_FILES: u64 = 500_000;
const MAX_MIGRATION_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const REQUIRED_HEADERS: [&str; 17] = [
    "Profile name",
    "Platform",
    "User-defined platform domain name",
    "Login account",
    "Login password",
    "2FA key",
    "Password protection",
    "Profile ID",
    "Cookie",
    "Proxy information",
    "Proxy Number",
    "Profile group",
    "Profile tag",
    "Profile note",
    "Custom number",
    "UA",
    "End-to-end encryption",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoreLoginCandidate {
    pub row_number: usize,
    pub source_profile_id: String,
    pub name: String,
    pub tags: Vec<String>,
    pub has_proxy: bool,
    pub has_login_data: bool,
    pub has_cookie: bool,
    pub has_custom_ua: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoreLoginPreview {
    pub source_name: String,
    pub profile_count: usize,
    pub sensitive_profile_count: usize,
    pub candidates: Vec<MoreLoginCandidate>,
    pub notices: Vec<String>,
}

pub fn inspect_export(path: &str) -> Result<MoreLoginPreview, String> {
    let source = resolve_export_path(path);
    if source
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        != Some("xlsx".to_owned())
    {
        return Err("仅支持 MoreLogin 导出的 .xlsx 文件".to_owned());
    }
    let metadata = fs::metadata(&source).map_err(|_| "无法读取指定的导出文件".to_owned())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_EXPORT_BYTES {
        return Err("导出文件为空、不是普通文件或超过 32 MB 限制".to_owned());
    }
    let mut workbook = open_workbook_auto(&source).map_err(|_| "无法解析 XLSX 文件".to_owned())?;
    let range = workbook
        .worksheet_range_at(0)
        .ok_or_else(|| "XLSX 中没有环境工作表".to_owned())?
        .map_err(|_| "无法读取环境工作表".to_owned())?;
    let mut rows = range.rows();
    let header = rows.next().ok_or_else(|| "环境工作表为空".to_owned())?;
    for (index, expected) in REQUIRED_HEADERS.iter().enumerate() {
        if header
            .get(index)
            .map(ToString::to_string)
            .unwrap_or_default()
            .trim()
            != *expected
        {
            return Err(format!("第 {} 列不是预期的 MoreLogin 字段", index + 1));
        }
    }
    let mut candidates = Vec::new();
    for (offset, row) in rows.enumerate() {
        let value = |index: usize| row.get(index).map(ToString::to_string).unwrap_or_default();
        let raw_name = value(0);
        if raw_name.trim().is_empty() && row.iter().all(|cell| cell.to_string().trim().is_empty()) {
            continue;
        }
        let source_profile_id = value(7).trim().to_owned();
        if source_profile_id.len() < 8
            || source_profile_id.len() > 32
            || !source_profile_id
                .bytes()
                .all(|value| value.is_ascii_digit())
        {
            return Err(format!(
                "第 {} 行缺少有效的 MoreLogin Profile ID",
                offset + 2
            ));
        }
        let name = truncate(
            if raw_name.trim().is_empty() {
                format!("MoreLogin 环境 {}", offset + 2)
            } else {
                raw_name.trim().to_owned()
            },
            128,
        );
        let mut unique_tags = BTreeSet::new();
        let tags = [value(11), value(12), value(1), "MoreLogin 导入".to_owned()]
            .into_iter()
            .map(|tag| truncate(tag.trim().to_owned(), 64))
            .filter(|tag| !tag.is_empty() && unique_tags.insert(tag.to_ascii_lowercase()))
            .collect::<Vec<_>>();
        let has_proxy = !value(9).trim().is_empty() || !value(10).trim().is_empty();
        let has_login_data = [3usize, 4, 5, 6]
            .into_iter()
            .any(|index| !value(index).trim().is_empty());
        let has_cookie = !value(8).trim().is_empty();
        let has_custom_ua = !value(15).trim().is_empty();
        let mut warnings = Vec::new();
        if has_proxy {
            warnings.push("代理配置将在后续安全凭据步骤中处理".to_owned());
        }
        if has_login_data {
            warnings.push("账号、密码和 2FA 不在元数据导入范围内".to_owned());
        }
        if has_cookie {
            warnings.push("Cookie 仅能通过完整环境迁移处理".to_owned());
        }
        if has_custom_ua {
            warnings.push("自定义 UA 已忽略，避免产生不一致的浏览器指纹".to_owned());
        }
        candidates.push(MoreLoginCandidate {
            row_number: offset + 2,
            source_profile_id,
            name,
            tags,
            has_proxy,
            has_login_data,
            has_cookie,
            has_custom_ua,
            warnings,
        });
    }
    let sensitive_profile_count = candidates
        .iter()
        .filter(|item| {
            item.has_proxy || item.has_login_data || item.has_cookie || item.has_custom_ua
        })
        .count();
    Ok(MoreLoginPreview {
        source_name: source
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("MoreLogin export")
            .to_owned(),
        profile_count: candidates.len(),
        sensitive_profile_count,
        candidates,
        notices: vec![
            "元数据导入仅处理环境名称、分组、标签和平台分类".to_owned(),
            "完整迁移会单独预检 History、Cookies、LocalStorage 和 IndexedDB".to_owned(),
            "只统计包含实际单元格内容的记录；预格式化空白行不会导入".to_owned(),
        ],
    })
}

fn resolve_export_path(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.extension().is_none() {
        path.with_extension("xlsx")
    } else {
        path
    }
}

#[derive(Debug)]
pub struct MoreLoginProfileDataMigrator {
    source_root: PathBuf,
    target_root: PathBuf,
}

impl MoreLoginProfileDataMigrator {
    pub fn new(
        source_root: impl Into<PathBuf>,
        target_root: impl Into<PathBuf>,
    ) -> PortResult<Self> {
        let source_root = source_root.into();
        if !source_root.is_absolute() || source_root.to_string_lossy().contains('\0') {
            return Err(PortError::new(
                PortErrorKind::InvalidData,
                "MoreLogin cache root must be absolute",
            ));
        }
        let target_root = create_and_canonicalize(target_root.into(), "Brows profile root")?;
        Ok(Self {
            source_root,
            target_root,
        })
    }

    fn source_dir(&self, source_profile_id: &str) -> PortResult<PathBuf> {
        validate_numeric_id(source_profile_id)?;
        let source_root = canonical_directory(self.source_root.clone(), "MoreLogin cache root")?;
        let path = canonical_directory(
            source_root.join(format!("chrome_{source_profile_id}")),
            "MoreLogin profile directory",
        )?;
        ensure_within(&path, &source_root, "MoreLogin profile directory")?;
        Ok(path)
    }

    fn profile_dir(&self, profile_id: &ProfileId) -> PortResult<PathBuf> {
        let key = hex_digest(profile_id.as_str().as_bytes());
        let path = self.target_root.join(key);
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if recovery::is_link(&metadata) || !metadata.is_dir() {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "target profile directory is linked or invalid",
                ));
            }
        }
        fs::create_dir_all(&path)
            .map_err(|error| io_error("create target profile directory", error))?;
        let path = path
            .canonicalize()
            .map_err(|error| io_error("canonicalize target profile directory", error))?;
        ensure_within(&path, &self.target_root, "target profile directory")?;
        Ok(path)
    }
}

impl LegacyProfileDataMigrator for MoreLoginProfileDataMigrator {
    fn inspect(
        &self,
        _profile_id: &ProfileId,
        source_profile_id: &str,
    ) -> PortResult<LegacyProfileDataSummary> {
        let source = self.source_dir(source_profile_id)?;
        let (file_count, total_bytes) = inspect_tree(&source)?;
        Ok(LegacyProfileDataSummary {
            source_profile_id: source_profile_id.to_owned(),
            file_count,
            total_bytes,
            has_history: source.join("Default/History").is_file(),
            has_cookies: source.join("Default/Network/Cookies").is_file(),
            has_login_data: source.join("Default/Login Data").is_file(),
            has_local_storage: source.join("Default/Local Storage").is_dir(),
            has_indexed_db: source.join("Default/IndexedDB").is_dir(),
            source_may_be_running: source_may_be_running(&source),
        })
    }

    fn migrate(
        &self,
        profile_id: &ProfileId,
        source_profile_id: &str,
        operation_key: &str,
    ) -> PortResult<LegacyMigrationArtifact> {
        validate_operation_key(operation_key)?;
        let source = self.source_dir(source_profile_id)?;
        if source_may_be_running(&source) {
            return Err(PortError::new(
                PortErrorKind::RuntimeInUse,
                "MoreLogin source environment appears to be running",
            ));
        }
        let profile_dir = self.profile_dir(profile_id)?;
        let target = profile_dir.join("userdata");
        let staging = profile_dir.join(format!("userdata.staging-{operation_key}"));
        let backup_name = format!("userdata.backup-{operation_key}");
        let backup = profile_dir.join(&backup_name);
        ensure_confined_child(&staging, &profile_dir)?;
        ensure_confined_child(&backup, &profile_dir)?;
        if staging.exists() || backup.exists() {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "migration staging or backup already exists",
            ));
        }
        if !recovery::checked_tree(&target)? {
            fs::create_dir_all(&target)
                .map_err(|error| io_error("create empty target profile", error))?;
        }
        let source_summary = inspect_tree(&source)?;
        if let Err(error) = copy_tree(&source, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        let copied_summary = inspect_tree(&staging)?;
        if copied_summary != source_summary {
            let _ = fs::remove_dir_all(&staging);
            return Err(PortError::new(
                PortErrorKind::Tampered,
                "staged profile copy did not match source size",
            ));
        }
        remove_transient_locks(&staging)?;
        fs::rename(&target, &backup)
            .map_err(|error| io_error("move existing profile to backup", error))?;
        if let Err(error) = fs::rename(&staging, &target) {
            let _ = fs::rename(&backup, &target);
            return Err(io_error("activate staged profile", error));
        }
        Ok(LegacyMigrationArtifact {
            backup_key: backup_name,
            copied_files: source_summary.0,
            copied_bytes: source_summary.1,
        })
    }

    fn rollback(&self, profile_id: &ProfileId, backup_key: &str) -> PortResult<()> {
        self.recover_rollback(profile_id, backup_key)
    }

    fn recover(&self, profile_id: &ProfileId, operation_key: &str) -> PortResult<()> {
        self.recover_migration(profile_id, operation_key)
    }
}

fn source_may_be_running(root: &Path) -> bool {
    [
        "SingletonLock",
        "SingletonCookie",
        "SingletonSocket",
        "DevToolsActivePort",
    ]
    .iter()
    .any(|name| root.join(name).exists())
}

fn copy_tree(source: &Path, target: &Path) -> PortResult<()> {
    fs::create_dir(target).map_err(|error| io_error("create staging directory", error))?;
    let mut stack = vec![(source.to_path_buf(), target.to_path_buf())];
    let mut files = 0u64;
    let mut bytes = 0u64;
    while let Some((from, to)) = stack.pop() {
        for entry in
            fs::read_dir(&from).map_err(|error| io_error("read source directory", error))?
        {
            let entry = entry.map_err(|error| io_error("read source entry", error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| io_error("read source entry type", error))?;
            if recovery::is_link(
                &fs::symlink_metadata(entry.path())
                    .map_err(|e| io_error("inspect migration link", e))?,
            ) {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "migration contains a link or reparse point",
                ));
            }
            let destination = to.join(entry.file_name());
            if file_type.is_dir() {
                fs::create_dir(&destination)
                    .map_err(|error| io_error("create copied directory", error))?;
                stack.push((entry.path(), destination));
            } else if file_type.is_file() {
                let metadata = entry
                    .metadata()
                    .map_err(|error| io_error("read source file metadata", error))?;
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(metadata.len());
                if files > MAX_MIGRATION_FILES || bytes > MAX_MIGRATION_BYTES {
                    return Err(PortError::new(
                        PortErrorKind::InvalidData,
                        "profile migration exceeds configured size limits",
                    ));
                }
                fs::copy(entry.path(), destination)
                    .map_err(|error| io_error("copy profile file", error))?;
            }
        }
    }
    Ok(())
}

fn inspect_tree(root: &Path) -> PortResult<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in
            fs::read_dir(path).map_err(|error| io_error("inspect profile directory", error))?
        {
            let entry = entry.map_err(|error| io_error("inspect profile entry", error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| io_error("inspect profile entry type", error))?;
            if recovery::is_link(
                &fs::symlink_metadata(entry.path())
                    .map_err(|e| io_error("inspect migration link", e))?,
            ) {
                return Err(PortError::new(
                    PortErrorKind::InvalidData,
                    "migration contains a link or reparse point",
                ));
            }
            let metadata = entry
                .metadata()
                .map_err(|error| io_error("inspect profile metadata", error))?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(metadata.len());
                if files > MAX_MIGRATION_FILES || bytes > MAX_MIGRATION_BYTES {
                    return Err(PortError::new(
                        PortErrorKind::InvalidData,
                        "profile migration exceeds configured size limits",
                    ));
                }
            }
        }
    }
    Ok((files, bytes))
}

fn remove_transient_locks(root: &Path) -> PortResult<()> {
    for name in [
        "SingletonLock",
        "SingletonCookie",
        "SingletonSocket",
        "DevToolsActivePort",
        "lockfile",
    ] {
        let path = root.join(name);
        if path.exists() {
            fs::remove_file(path).map_err(|error| io_error("remove copied lock file", error))?;
        }
    }
    Ok(())
}

fn canonical_directory(path: PathBuf, label: &'static str) -> PortResult<PathBuf> {
    let path = path
        .canonicalize()
        .map_err(|error| io_error("canonicalize directory", error))?;
    if !path.is_dir() {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("{label} is not a directory"),
        ));
    }
    Ok(path)
}

fn create_and_canonicalize(path: PathBuf, label: &'static str) -> PortResult<PathBuf> {
    fs::create_dir_all(&path).map_err(|error| io_error("create managed directory", error))?;
    canonical_directory(path, label)
}

fn ensure_within(path: &Path, root: &Path, label: &'static str) -> PortResult<()> {
    if path == root || !path.starts_with(root) {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            format!("{label} escaped its managed root"),
        ));
    }
    Ok(())
}

fn ensure_confined_child(path: &Path, root: &Path) -> PortResult<()> {
    if path.parent() != Some(root)
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "migration path escaped profile root",
        ));
    }
    Ok(())
}

fn validate_numeric_id(value: &str) -> PortResult<()> {
    if value.len() < 8 || value.len() > 32 || !value.bytes().all(|value| value.is_ascii_digit()) {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "invalid MoreLogin profile identifier",
        ));
    }
    Ok(())
}

fn validate_operation_key(value: &str) -> PortResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "invalid migration operation key",
        ));
    }
    Ok(())
}

fn validate_backup_key(value: &str) -> PortResult<()> {
    let Some(operation) = value.strip_prefix("userdata.backup-") else {
        return Err(PortError::new(
            PortErrorKind::InvalidData,
            "invalid migration backup key",
        ));
    };
    validate_operation_key(operation)
}

fn hex_digest(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn truncate(value: String, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn io_error(context: &'static str, error: std::io::Error) -> PortError {
    let kind = match error.kind() {
        std::io::ErrorKind::NotFound => PortErrorKind::NotFound,
        std::io::ErrorKind::PermissionDenied => PortErrorKind::Locked,
        _ => PortErrorKind::Unavailable,
    };
    PortError::new(kind, format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("brows-morelogin-{label}-{nonce}"))
    }

    #[test]
    fn migration_copies_swaps_and_rolls_back_without_deleting_either_version() {
        let root = temp_root("swap");
        let source_root = root.join("source");
        let target_root = root.join("target");
        let source = source_root.join("chrome_12345678/Default/Network");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("Cookies"), b"encrypted-cookie").unwrap();
        fs::write(source.parent().unwrap().join("History"), b"history").unwrap();
        fs::create_dir_all(&target_root).unwrap();
        let profile = ProfileId::new("profile-one").unwrap();
        let profile_dir = target_root.join(hex_digest(profile.as_str().as_bytes()));
        fs::create_dir_all(profile_dir.join("userdata/Default")).unwrap();
        fs::write(profile_dir.join("userdata/Default/History"), b"original").unwrap();
        let migrator = MoreLoginProfileDataMigrator::new(&source_root, &target_root).unwrap();
        let artifact = migrator
            .migrate(&profile, "12345678", "migration-1")
            .unwrap();
        assert_eq!(
            fs::read(profile_dir.join("userdata/Default/History")).unwrap(),
            b"history"
        );
        migrator.rollback(&profile, &artifact.backup_key).unwrap();
        assert_eq!(
            fs::read(profile_dir.join("userdata/Default/History")).unwrap(),
            b"original"
        );
        assert!(profile_dir
            .join("userdata.after-rollback-migration-1")
            .is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn traversal_like_identifiers_and_backup_keys_are_rejected() {
        assert!(validate_numeric_id("../../evil").is_err());
        assert!(validate_operation_key("../evil").is_err());
        assert!(validate_backup_key("C:\\userdata").is_err());
    }

    #[test]
    fn omitted_xlsx_extension_is_resolved_without_changing_directories() {
        assert_eq!(
            resolve_export_path(r"D:\export_profile"),
            PathBuf::from(r"D:\export_profile.xlsx")
        );
        assert_eq!(
            resolve_export_path(r"D:\exports\profile.xlsx"),
            PathBuf::from(r"D:\exports\profile.xlsx")
        );
    }
}
