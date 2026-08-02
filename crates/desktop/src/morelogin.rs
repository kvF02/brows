use calamine::{open_workbook_auto, Reader};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
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
pub(crate) struct MoreLoginCandidate {
    pub row_number: usize,
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
pub(crate) struct MoreLoginPreview {
    pub source_name: String,
    pub profile_count: usize,
    pub sensitive_profile_count: usize,
    pub candidates: Vec<MoreLoginCandidate>,
    pub notices: Vec<String>,
}

pub(crate) fn inspect(path: &str) -> Result<MoreLoginPreview, String> {
    let source = Path::new(path);
    if source
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        != Some("xlsx".to_owned())
    {
        return Err("仅支持 MoreLogin 导出的 .xlsx 文件".to_owned());
    }
    let metadata = std::fs::metadata(source).map_err(|_| "无法读取指定的导出文件".to_owned())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_SOURCE_BYTES {
        return Err("导出文件为空、不是普通文件或超过 32 MB 限制".to_owned());
    }

    let mut workbook = open_workbook_auto(source).map_err(|_| "无法解析 XLSX 文件".to_owned())?;
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
        let name = truncate(
            if raw_name.trim().is_empty() {
                format!("MoreLogin 环境 {}", offset + 2)
            } else {
                raw_name.trim().to_owned()
            },
            128,
        );
        let mut unique_tags = BTreeSet::new();
        let tags = [value(11), value(12), value(1)]
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
            warnings.push("账号、密码和 2FA 不在本次导入范围内".to_owned());
        }
        if has_cookie {
            warnings.push("Cookie 不在本次导入范围内，登录态不会自动迁移".to_owned());
        }
        if has_custom_ua {
            warnings.push("自定义 UA 已忽略，避免产生不一致的浏览器指纹".to_owned());
        }
        candidates.push(MoreLoginCandidate {
            row_number: offset + 2,
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
            "本阶段仅导入环境名称、分组、标签和平台分类".to_owned(),
            "不会导入或记录密码、2FA、Cookie、代理凭据和自定义 UA".to_owned(),
            "XLSX 不包含 LocalStorage、IndexedDB 和扩展数据，因此无法完全复原原登录环境".to_owned(),
        ],
    })
}

fn truncate(value: String, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_unicode_characters() {
        assert_eq!(truncate("测试环境ABC".to_owned(), 5), "测试环境A");
    }

    #[test]
    fn candidate_debug_never_has_sensitive_value_fields() {
        let candidate = MoreLoginCandidate {
            row_number: 2,
            name: "test".to_owned(),
            tags: vec![],
            has_proxy: true,
            has_login_data: true,
            has_cookie: true,
            has_custom_ua: true,
            warnings: vec![],
        };
        let debug = format!("{candidate:?}");
        assert!(!debug.contains("password"));
        assert!(!debug.contains("cookie="));
    }

    #[test]
    fn inspects_external_fixture_when_configured() {
        let Ok(path) = std::env::var("BROWS_MORELOGIN_FIXTURE") else {
            return;
        };
        let preview = inspect(&path).expect("configured MoreLogin fixture should parse");
        assert!(preview.profile_count > 0);
        assert_eq!(preview.profile_count, preview.candidates.len());
        assert!(preview
            .candidates
            .iter()
            .all(|item| !item.name.trim().is_empty()));
    }
}
