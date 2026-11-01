use super::*;
use serde::{Deserialize, Serialize};
use std::io::{Seek, Write};

pub(super) const MAX_FILES: usize = 4096;
pub(super) const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileHash {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub(super) struct BundleIntegrity {
    pub root: PathBuf,
    pub files: Vec<FileHash>,
}

impl BundleIntegrity {
    pub fn verify(&self) -> PortResult<()> {
        let actual = scan(&self.root, &|| Ok(()))?;
        if actual != self.files {
            return Err(untrusted(
                "浏览器安装文件已变化，请检查安装或安装新的稳定版",
            ));
        }
        Ok(())
    }
}

pub(super) fn untrusted(message: &str) -> PortError {
    PortError::new(PortErrorKind::RuntimeUntrusted, message)
}

pub(super) fn safe_relative(value: &str) -> PortResult<PathBuf> {
    if value.is_empty() || value.len() > 200 || value.contains('\\') {
        return Err(invalid_data("invalid package path"));
    }
    for part in value.split('/') {
        let base = part
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with('.')
            || part.starts_with(' ')
            || part.ends_with(' ')
            || !part
                .bytes()
                // Official CFT includes "First Run". Interior spaces are
                // ordinary Windows filename characters; edge spaces and
                // reserved-device aliases must still be rejected.
                .all(|b| b.is_ascii_alphanumeric() || b" ._-".contains(&b))
            || [
                "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
                "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
                "LPT9",
            ]
            .contains(&base.as_str())
        {
            return Err(invalid_data("unsafe package path"));
        }
    }
    Ok(PathBuf::from(value))
}

pub(super) fn regular_directory(path: &Path) -> PortResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| invalid_data("cannot inspect managed runtime directory"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(untrusted("运行时目录包含链接或不是目录"));
    }
    Ok(())
}

pub(super) fn scan(
    root: &Path,
    checkpoint: &dyn Fn() -> PortResult<()>,
) -> PortResult<Vec<FileHash>> {
    regular_directory(root)?;
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut bytes = 0u64;
    let mut entries = 0usize;
    while let Some(path) = stack.pop() {
        checkpoint()?;
        for entry in
            fs::read_dir(path).map_err(|_| invalid_data("cannot read runtime directory"))?
        {
            checkpoint()?;
            entries += 1;
            if entries > MAX_FILES {
                return Err(invalid_data("too many runtime entries"));
            }
            let entry = entry.map_err(|_| invalid_data("cannot read runtime entry"))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| invalid_data("cannot inspect runtime file"))?;
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return Err(untrusted("运行时包含链接或 reparse point"));
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| invalid_data("runtime file escaped bundle"))?
                .to_str()
                .ok_or_else(|| invalid_data("invalid runtime file name"))?
                .replace('\\', "/");
            safe_relative(&relative)?;
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| invalid_data("runtime size overflow"))?;
                if bytes > MAX_BYTES {
                    return Err(invalid_data("runtime bundle exceeds size limit"));
                }
                files.push(FileHash {
                    path: relative,
                    bytes: metadata.len(),
                    sha256: hash_checked(&path, checkpoint)?,
                });
            } else {
                return Err(untrusted("运行时包含不支持的文件类型"));
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut names = HashSet::new();
    for file in &files {
        if !names.insert(file.path.to_ascii_lowercase()) {
            return Err(invalid_data("case-colliding runtime files"));
        }
    }
    Ok(files)
}

fn hash_checked(path: &Path, checkpoint: &dyn Fn() -> PortResult<()>) -> PortResult<String> {
    let mut file = File::open(path).map_err(|_| invalid_data("cannot read runtime file"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        checkpoint()?;
        let n = file
            .read(&mut buffer)
            .map_err(|_| invalid_data("cannot hash runtime file"))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn extract<R: Read + Seek>(
    input: R,
    root: &Path,
    checkpoint: &dyn Fn() -> PortResult<()>,
) -> PortResult<Vec<FileHash>> {
    let mut archive =
        zip::ZipArchive::new(input).map_err(|_| invalid_data("浏览器安装包不是有效 ZIP"))?;
    if archive.len() > MAX_FILES || archive.is_empty() {
        return Err(invalid_data("invalid runtime archive entry count"));
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        checkpoint()?;
        let mut entry = archive
            .by_index(index)
            .map_err(|_| invalid_data("cannot read runtime archive"))?;
        if entry.encrypted()
            || entry.is_symlink()
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
        {
            return Err(invalid_data("unsupported runtime ZIP entry"));
        }
        if entry.unix_mode().is_some_and(|mode| {
            mode & 0o170000 != 0 && !matches!(mode & 0o170000, 0o100000 | 0o040000)
        }) {
            return Err(invalid_data("unsupported runtime entry type"));
        }
        let name = entry.name().trim_end_matches('/');
        if name == "chrome-win64" && entry.is_dir() {
            continue;
        }
        let name = name
            .strip_prefix("chrome-win64/")
            .ok_or_else(|| invalid_data("unexpected runtime package root"))?;
        let relative = safe_relative(name)?;
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(invalid_data("duplicate runtime archive path"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| invalid_data("runtime archive size overflow"))?;
        if total > MAX_BYTES {
            return Err(invalid_data("runtime archive exceeds size limit"));
        }
        let path = root.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(path)
                .map_err(|_| invalid_data("cannot create runtime directory"))?;
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|_| invalid_data("cannot create runtime parent"))?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|_| invalid_data("cannot create runtime file"))?;
            let expected = entry.size();
            let mut copied = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                checkpoint()?;
                let n = entry
                    .read(&mut buffer)
                    .map_err(|_| invalid_data("runtime ZIP validation failed"))?;
                if n == 0 {
                    break;
                }
                copied += n as u64;
                if copied > expected {
                    return Err(invalid_data("runtime entry grew beyond declared size"));
                }
                file.write_all(&buffer[..n])
                    .map_err(|_| invalid_data("cannot write runtime file"))?;
            }
            if copied != expected {
                return Err(invalid_data("truncated runtime entry"));
            }
            file.sync_all()
                .map_err(|_| internal("cannot sync runtime file"))?;
        }
    }
    let files = scan(root, checkpoint)?;
    for required in [
        "chrome.exe",
        "chrome.dll",
        "chrome_elf.dll",
        "resources.pak",
        "icudtl.dat",
    ] {
        if !files.iter().any(|file| file.path == required) {
            return Err(invalid_data("浏览器安装包缺少必要文件"));
        }
    }
    Ok(files)
}
