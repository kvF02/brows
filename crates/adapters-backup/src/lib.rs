#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

//! Authenticated, streaming local `.browsbak` archives.

use argon2::{Algorithm, Argon2, Params, Version};
use brows_application::{
    BackupExportSpec, BackupImportPayload, BackupImportSpec, BackupInspection, BackupService,
    PortError, PortErrorKind, PortResult,
};
use brows_contracts::ProfileDto;
use chacha20poly1305::{
    aead::{
        stream::{DecryptorBE32, EncryptorBE32},
        KeyInit, Payload,
    },
    XChaCha20Poly1305,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"BROWSBAK";
const PAYLOAD_VERSION: u16 = 1;
const ARGON_MEMORY_KIB: u32 = 64 * 1024;
const ARGON_ITERATIONS: u32 = 3;
const ARGON_LANES: u32 = 1;
const MAX_ARGON_MEMORY_KIB: u32 = 256 * 1024;
const MAX_ARGON_ITERATIONS: u32 = 10;
const MAX_ARGON_LANES: u32 = 8;
const SALT_BYTES: usize = 16;
const STREAM_NONCE_BYTES: usize = 19;
const VERIFIER_BYTES: usize = 32;
const HEADER_PREFIX_BYTES: usize = 8 + 2 + 4 + 4 + 4 + SALT_BYTES + STREAM_NONCE_BYTES;
const HEADER_BYTES: usize = HEADER_PREFIX_BYTES + VERIFIER_BYTES;
const CHUNK_BYTES: usize = 1024 * 1024;
const TAG_BYTES: usize = 16;
const MAX_PASSWORD_BYTES: usize = 1024;
const MIN_PASSWORD_BYTES: usize = 12;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_FILES: u64 = 200_000;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024 * 1024;

type DerivedKey = Zeroizing<[u8; 32]>;
type HeaderKeyNonce = (Vec<u8>, DerivedKey, [u8; STREAM_NONCE_BYTES]);

#[derive(Debug)]
pub struct EncryptedBackupService {
    backup_root: PathBuf,
    profile_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupArchiveEntry {
    pub name: String,
    pub size_bytes: u64,
    pub modified_at_ms: Option<u64>,
}

impl EncryptedBackupService {
    pub fn new(
        backup_root: impl Into<PathBuf>,
        profile_root: impl Into<PathBuf>,
    ) -> PortResult<Self> {
        let backup_root = create_and_canonicalize(backup_root.into(), "backup root")?;
        let profile_root = create_and_canonicalize(profile_root.into(), "profile root")?;
        Ok(Self {
            backup_root,
            profile_root,
        })
    }

    pub fn archive_size(&self, name: &str) -> PortResult<u64> {
        Ok(fs::metadata(self.archive_path(name)?)
            .map_err(|error| io_error("read backup metadata", error))?
            .len())
    }

    /// Lists completed archives in the managed backup directory. Partial
    /// files are intentionally omitted so interrupted exports are never
    /// presented as usable backups.
    pub fn list_archives(&self) -> PortResult<Vec<BackupArchiveEntry>> {
        let mut entries = Vec::new();
        let read_dir =
            fs::read_dir(&self.backup_root).map_err(|error| io_error("list backups", error))?;
        for item in read_dir {
            let item = item.map_err(|error| io_error("read backup entry", error))?;
            let metadata = item
                .metadata()
                .map_err(|error| io_error("read backup metadata", error))?;
            if !metadata.is_file() {
                continue;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if validate_archive_name(&name).is_err() {
                continue;
            }
            let modified_at_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|value| u64::try_from(value.as_millis()).ok());
            entries.push(BackupArchiveEntry {
                name,
                size_bytes: metadata.len(),
                modified_at_ms,
            });
        }
        entries.sort_by(|left, right| {
            right
                .modified_at_ms
                .cmp(&left.modified_at_ms)
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(entries)
    }

    fn archive_path(&self, name: &str) -> PortResult<PathBuf> {
        validate_archive_name(name)?;
        Ok(self.backup_root.join(name))
    }

    fn profile_data_path(&self, profile_id: &str) -> PathBuf {
        let digest = Sha256::digest(profile_id.as_bytes());
        self.profile_root
            .join(format!("{digest:x}"))
            .join("userdata")
    }

    fn export_archive(&self, spec: &BackupExportSpec) -> PortResult<()> {
        validate_password(spec.password.expose())?;
        if spec.include_proxy_credential {
            return Err(PortError::new(
                PortErrorKind::CapabilityUnsupported,
                "proxy credential export requires a separate credential envelope and is not enabled",
            ));
        }

        let destination = self.archive_path(&spec.destination_name)?;
        if destination.exists() {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "backup destination already exists",
            ));
        }
        let partial = self
            .backup_root
            .join(format!("{}.partial", spec.destination_name));
        if partial.exists() {
            return Err(PortError::new(
                PortErrorKind::AlreadyExists,
                "an incomplete backup with the same name already exists",
            ));
        }

        let source = self.profile_data_path(spec.profile.id.as_str());
        if source.exists() {
            inspect_source_tree(&source, &self.profile_root)?;
        }

        let result = (|| {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial)
                .map_err(|error| io_error("create partial backup", error))?;
            let (header, key, nonce) = build_header(spec.password.expose())?;
            output
                .write_all(&header)
                .map_err(|error| io_error("write backup header", error))?;

            let encrypted = EncryptedWriter::new(output, &key, &nonce, header);
            let mut archive = Builder::new(encrypted);
            archive.follow_symlinks(false);
            append_manifest(&mut archive, &spec.profile)?;
            if source.exists() {
                archive
                    .append_dir_all("userdata", &source)
                    .map_err(|error| io_error("archive profile data", error))?;
            }
            archive
                .finish()
                .map_err(|error| io_error("finish backup payload", error))?;
            let encrypted = archive
                .into_inner()
                .map_err(|error| io_error("finalize backup archive", error))?;
            let mut output = encrypted.finish()?;
            output
                .flush()
                .map_err(|error| io_error("flush encrypted backup", error))?;
            output
                .sync_all()
                .map_err(|error| io_error("sync encrypted backup", error))?;
            fs::rename(&partial, &destination)
                .map_err(|error| io_error("install encrypted backup", error))?;
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&partial);
        }
        result
    }

    fn import_archive(&self, spec: &BackupImportSpec) -> PortResult<BackupImportPayload> {
        validate_password(spec.password.expose())?;
        let source = self.archive_path(&spec.source_name)?;
        let mut input = File::open(&source).map_err(|error| io_error("open backup", error))?;
        let (header, key, nonce) = read_and_verify_header(&mut input, spec.password.expose())?;
        let encrypted = EncryptedReader::new(input, &key, &nonce, header);
        let mut archive = Archive::new(encrypted);

        let staging_parent = create_and_canonicalize(
            self.backup_root.join("restore-staging"),
            "backup restore staging root",
        )?;
        let staging = unique_staging_directory(&staging_parent)?;
        let result = extract_archive(&mut archive, &staging).and_then(|profile| {
            let mut encrypted = archive.into_inner();
            encrypted.finish_authentication()?;
            let restored_data_dir = staging.join("userdata");
            fs::create_dir_all(&restored_data_dir)
                .map_err(|error| io_error("create restored profile directory", error))?;
            Ok(BackupImportPayload {
                profile,
                restored_data_dir,
            })
        });
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }
}

impl BackupService for EncryptedBackupService {
    fn inspect(&self, source_name: &str) -> PortResult<BackupInspection> {
        let mut input = File::open(self.archive_path(source_name)?)
            .map_err(|error| io_error("open backup", error))?;
        let header = read_header(&mut input)?;
        Ok(BackupInspection {
            format_version: header.version,
            profile_name: "Encrypted Brows profile".to_owned(),
            includes_proxy_credential: false,
        })
    }

    fn export(&self, spec: &BackupExportSpec) -> PortResult<()> {
        self.export_archive(spec)
    }

    fn import(&self, spec: &BackupImportSpec) -> PortResult<BackupImportPayload> {
        self.import_archive(spec)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupManifest {
    format_version: u16,
    profile: ProfileDto,
    includes_proxy_credential: bool,
}

fn append_manifest<W: Write>(
    archive: &mut Builder<W>,
    profile: &brows_domain::ProfileSpec,
) -> PortResult<()> {
    let manifest = serde_json::to_vec(&BackupManifest {
        format_version: PAYLOAD_VERSION,
        profile: ProfileDto::from(profile),
        includes_proxy_credential: false,
    })
    .map_err(|error| invalid(format!("serialize backup manifest: {error}")))?;
    let mut header = Header::new_gnu();
    header.set_size(u64::try_from(manifest.len()).unwrap_or(u64::MAX));
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_cksum();
    archive
        .append_data(&mut header, "manifest.json", Cursor::new(manifest))
        .map_err(|error| io_error("archive backup manifest", error))
}

fn extract_archive<R: Read>(
    archive: &mut Archive<R>,
    staging: &Path,
) -> PortResult<brows_domain::ProfileSpec> {
    let mut manifest = None;
    let mut file_count = 0_u64;
    let mut total_bytes = 0_u64;
    let entries = archive
        .entries()
        .map_err(|error| tampered(format!("read backup entries: {error}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| tampered(format!("read backup entry: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| tampered(format!("read backup path: {error}")))?
            .into_owned();
        validate_archive_entry_path(&path)?;
        let entry_type = entry.header().entry_type();
        if path == Path::new("manifest.json") {
            if manifest.is_some() || !entry_type.is_file() || entry.size() > MAX_MANIFEST_BYTES {
                return Err(tampered("backup manifest is invalid"));
            }
            let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| tampered(format!("read backup manifest: {error}")))?;
            let parsed: BackupManifest = serde_json::from_slice(&bytes)
                .map_err(|error| tampered(format!("parse backup manifest: {error}")))?;
            if parsed.format_version != PAYLOAD_VERSION || parsed.includes_proxy_credential {
                return Err(PortError::new(
                    PortErrorKind::Incompatible,
                    "backup manifest version or credential mode is unsupported",
                ));
            }
            manifest = Some(parsed.profile.try_into().map_err(
                |error: brows_contracts::ContractError| {
                    tampered(format!("invalid profile in backup: {error}"))
                },
            )?);
            continue;
        }

        if !path.starts_with("userdata") {
            return Err(tampered("backup contains an unexpected top-level entry"));
        }
        file_count = file_count
            .checked_add(1)
            .ok_or_else(|| tampered("backup contains too many entries"))?;
        if file_count > MAX_ARCHIVE_FILES {
            return Err(tampered("backup contains too many entries"));
        }
        total_bytes = total_bytes
            .checked_add(entry.size())
            .ok_or_else(|| tampered("backup expanded size overflow"))?;
        if total_bytes > MAX_ARCHIVE_BYTES {
            return Err(tampered("backup expanded size exceeds the safety limit"));
        }

        let destination = staging.join(&path);
        ensure_within(&destination, staging, "restored backup entry")?;
        if entry_type.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| io_error("create restored directory", error))?;
        } else if entry_type.is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| io_error("create restored file parent", error))?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|error| io_error("create restored file", error))?;
            let copied = io::copy(&mut entry, &mut output)
                .map_err(|error| io_error("restore backup file", error))?;
            if copied != entry.size() {
                return Err(tampered(
                    "restored file size does not match its archive header",
                ));
            }
        } else {
            return Err(tampered("backup contains a link or unsupported file type"));
        }
    }
    manifest.ok_or_else(|| tampered("backup manifest is missing"))
}

#[derive(Debug)]
struct ParsedHeader {
    version: u16,
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
    salt: [u8; SALT_BYTES],
    nonce: [u8; STREAM_NONCE_BYTES],
    verifier: [u8; VERIFIER_BYTES],
    encoded: Vec<u8>,
}

fn build_header(password: &[u8]) -> PortResult<HeaderKeyNonce> {
    let mut salt = [0_u8; SALT_BYTES];
    let mut nonce = [0_u8; STREAM_NONCE_BYTES];
    getrandom::fill(&mut salt)
        .map_err(|error| unavailable(format!("generate backup salt: {error}")))?;
    getrandom::fill(&mut nonce)
        .map_err(|error| unavailable(format!("generate backup nonce: {error}")))?;
    let mut prefix = Vec::with_capacity(HEADER_PREFIX_BYTES);
    prefix.extend_from_slice(MAGIC);
    prefix.extend_from_slice(&PAYLOAD_VERSION.to_le_bytes());
    prefix.extend_from_slice(&ARGON_MEMORY_KIB.to_le_bytes());
    prefix.extend_from_slice(&ARGON_ITERATIONS.to_le_bytes());
    prefix.extend_from_slice(&ARGON_LANES.to_le_bytes());
    prefix.extend_from_slice(&salt);
    prefix.extend_from_slice(&nonce);
    let key = derive_key(
        password,
        &salt,
        ARGON_MEMORY_KIB,
        ARGON_ITERATIONS,
        ARGON_LANES,
    )?;
    let verifier = password_verifier(&key[..], &prefix);
    prefix.extend_from_slice(&verifier);
    Ok((prefix, key, nonce))
}

fn read_header(reader: &mut impl Read) -> PortResult<ParsedHeader> {
    let mut encoded = vec![0_u8; HEADER_BYTES];
    reader.read_exact(&mut encoded).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            tampered("backup header is truncated")
        } else {
            io_error("read backup header", error)
        }
    })?;
    if &encoded[..MAGIC.len()] != MAGIC {
        return Err(PortError::new(
            PortErrorKind::Incompatible,
            "file is not a Brows backup",
        ));
    }
    let version = u16::from_le_bytes(encoded[8..10].try_into().expect("header slice"));
    if version != PAYLOAD_VERSION {
        return Err(PortError::new(
            PortErrorKind::Incompatible,
            format!("backup format version {version} is unsupported"),
        ));
    }
    let memory_kib = u32::from_le_bytes(encoded[10..14].try_into().expect("header slice"));
    let iterations = u32::from_le_bytes(encoded[14..18].try_into().expect("header slice"));
    let lanes = u32::from_le_bytes(encoded[18..22].try_into().expect("header slice"));
    if memory_kib == 0
        || memory_kib > MAX_ARGON_MEMORY_KIB
        || iterations == 0
        || iterations > MAX_ARGON_ITERATIONS
        || lanes == 0
        || lanes > MAX_ARGON_LANES
    {
        return Err(PortError::new(
            PortErrorKind::Incompatible,
            "backup key-derivation parameters exceed safety limits",
        ));
    }
    let mut salt = [0; SALT_BYTES];
    salt.copy_from_slice(&encoded[22..22 + SALT_BYTES]);
    let nonce_start = 22 + SALT_BYTES;
    let mut nonce = [0; STREAM_NONCE_BYTES];
    nonce.copy_from_slice(&encoded[nonce_start..nonce_start + STREAM_NONCE_BYTES]);
    let mut verifier = [0; VERIFIER_BYTES];
    verifier.copy_from_slice(&encoded[HEADER_PREFIX_BYTES..HEADER_BYTES]);
    Ok(ParsedHeader {
        version,
        memory_kib,
        iterations,
        lanes,
        salt,
        nonce,
        verifier,
        encoded,
    })
}

fn read_and_verify_header(reader: &mut impl Read, password: &[u8]) -> PortResult<HeaderKeyNonce> {
    let parsed = read_header(reader)?;
    let key = derive_key(
        password,
        &parsed.salt,
        parsed.memory_kib,
        parsed.iterations,
        parsed.lanes,
    )?;
    let actual = password_verifier(&key[..], &parsed.encoded[..HEADER_PREFIX_BYTES]);
    if !constant_time_equal(&actual, &parsed.verifier) {
        return Err(PortError::new(
            PortErrorKind::WrongPassword,
            "backup password is incorrect or the public header was modified",
        ));
    }
    Ok((parsed.encoded, key, parsed.nonce))
}

fn derive_key(
    password: &[u8],
    salt: &[u8],
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
) -> PortResult<Zeroizing<[u8; 32]>> {
    let params = Params::new(memory_kib, iterations, lanes, Some(32))
        .map_err(|error| invalid(format!("invalid backup key parameters: {error}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0_u8; 32]);
    argon
        .hash_password_into(password, salt, key.as_mut())
        .map_err(|error| unavailable(format!("derive backup key: {error}")))?;
    Ok(key)
}

fn password_verifier(key: &[u8], header_prefix: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"brows-backup-password-verifier-v1\0");
    digest.update(key);
    digest.update(header_prefix);
    digest.finalize().into()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

struct EncryptedWriter<W: Write> {
    inner: W,
    encryptor: Option<EncryptorBE32<XChaCha20Poly1305>>,
    header: Vec<u8>,
    buffer: Vec<u8>,
}

impl<W: Write> EncryptedWriter<W> {
    fn new(inner: W, key: &[u8; 32], nonce: &[u8; STREAM_NONCE_BYTES], header: Vec<u8>) -> Self {
        let cipher = XChaCha20Poly1305::new(key.into());
        let encryptor = EncryptorBE32::from_aead(cipher, nonce.into());
        Self {
            inner,
            encryptor: Some(encryptor),
            header,
            buffer: Vec::with_capacity(CHUNK_BYTES),
        }
    }

    fn encrypt_next(&mut self) -> PortResult<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let cipher_len = self
            .buffer
            .len()
            .checked_add(TAG_BYTES)
            .ok_or_else(|| invalid("backup chunk length overflow"))?;
        let prefix = frame_prefix(0, cipher_len)?;
        let aad = frame_aad(&self.header, &prefix);
        let encrypted = self
            .encryptor
            .as_mut()
            .ok_or_else(|| invalid("backup encryptor is already finished"))?
            .encrypt_next(Payload {
                msg: &self.buffer,
                aad: &aad,
            })
            .map_err(|_| unavailable("encrypt backup chunk"))?;
        self.inner
            .write_all(&prefix)
            .and_then(|_| self.inner.write_all(&encrypted))
            .map_err(|error| io_error("write encrypted backup chunk", error))?;
        self.buffer.clear();
        Ok(())
    }

    fn finish(mut self) -> PortResult<W> {
        self.encrypt_next()?;
        let prefix = frame_prefix(1, TAG_BYTES)?;
        let aad = frame_aad(&self.header, &prefix);
        let encrypted = self
            .encryptor
            .take()
            .ok_or_else(|| invalid("backup encryptor is already finished"))?
            .encrypt_last(Payload {
                msg: &[],
                aad: &aad,
            })
            .map_err(|_| unavailable("finish backup encryption"))?;
        self.inner
            .write_all(&prefix)
            .and_then(|_| self.inner.write_all(&encrypted))
            .map_err(|error| io_error("write encrypted backup footer", error))?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for EncryptedWriter<W> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        let original = bytes.len();
        while !bytes.is_empty() {
            let available = CHUNK_BYTES - self.buffer.len();
            let copied = available.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..copied]);
            bytes = &bytes[copied..];
            if self.buffer.len() == CHUNK_BYTES {
                self.encrypt_next().map_err(port_to_io)?;
            }
        }
        Ok(original)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write> std::fmt::Debug for EncryptedWriter<W> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EncryptedWriter")
            .field("buffered_bytes", &self.buffer.len())
            .finish_non_exhaustive()
    }
}

struct EncryptedReader<R: Read> {
    inner: R,
    decryptor: Option<DecryptorBE32<XChaCha20Poly1305>>,
    header: Vec<u8>,
    plaintext: Cursor<Vec<u8>>,
    saw_final: bool,
}

impl<R: Read> EncryptedReader<R> {
    fn new(inner: R, key: &[u8; 32], nonce: &[u8; STREAM_NONCE_BYTES], header: Vec<u8>) -> Self {
        let cipher = XChaCha20Poly1305::new(key.into());
        let decryptor = DecryptorBE32::from_aead(cipher, nonce.into());
        Self {
            inner,
            decryptor: Some(decryptor),
            header,
            plaintext: Cursor::new(Vec::new()),
            saw_final: false,
        }
    }

    fn read_frame(&mut self) -> PortResult<bool> {
        if self.saw_final {
            return Ok(false);
        }
        let mut prefix = [0_u8; 5];
        self.inner.read_exact(&mut prefix).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                tampered("encrypted backup is truncated before its final frame")
            } else {
                io_error("read encrypted backup frame", error)
            }
        })?;
        let final_frame = match prefix[0] {
            0 => false,
            1 => true,
            _ => return Err(tampered("encrypted backup has an invalid frame type")),
        };
        let cipher_len = u32::from_le_bytes(prefix[1..5].try_into().expect("frame prefix"));
        let cipher_len = usize::try_from(cipher_len)
            .map_err(|_| tampered("encrypted backup frame is too large"))?;
        if !(TAG_BYTES..=CHUNK_BYTES + TAG_BYTES).contains(&cipher_len) {
            return Err(tampered("encrypted backup frame length is invalid"));
        }
        if final_frame && cipher_len != TAG_BYTES {
            return Err(tampered("encrypted backup final frame is invalid"));
        }
        let mut ciphertext = vec![0_u8; cipher_len];
        self.inner
            .read_exact(&mut ciphertext)
            .map_err(|error| tampered(format!("encrypted backup frame is truncated: {error}")))?;
        let aad = frame_aad(&self.header, &prefix);
        let plaintext = if final_frame {
            let decrypted = self
                .decryptor
                .take()
                .ok_or_else(|| tampered("encrypted backup has duplicate final frames"))?
                .decrypt_last(Payload {
                    msg: &ciphertext,
                    aad: &aad,
                })
                .map_err(|_| tampered("encrypted backup final authentication failed"))?;
            if !decrypted.is_empty() {
                return Err(tampered("encrypted backup final frame is not empty"));
            }
            self.saw_final = true;
            decrypted
        } else {
            self.decryptor
                .as_mut()
                .ok_or_else(|| tampered("encrypted backup has data after its final frame"))?
                .decrypt_next(Payload {
                    msg: &ciphertext,
                    aad: &aad,
                })
                .map_err(|_| tampered("encrypted backup authentication failed"))?
        };
        self.plaintext = Cursor::new(plaintext);
        Ok(!final_frame)
    }

    fn finish_authentication(&mut self) -> PortResult<()> {
        let mut drain = [0_u8; 8192];
        while self.read(&mut drain).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                tampered(error.to_string())
            } else {
                io_error("finish backup authentication", error)
            }
        })? != 0
        {}
        if !self.saw_final {
            return Err(tampered("encrypted backup final frame is missing"));
        }
        let mut trailing = [0_u8; 1];
        if self
            .inner
            .read(&mut trailing)
            .map_err(|error| io_error("check backup trailing data", error))?
            != 0
        {
            return Err(tampered("encrypted backup has trailing data"));
        }
        Ok(())
    }
}

impl<R: Read> Read for EncryptedReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            let read = self.plaintext.read(output)?;
            if read != 0 || output.is_empty() {
                return Ok(read);
            }
            if !self.read_frame().map_err(port_to_io)? {
                return Ok(0);
            }
        }
    }
}

impl<R: Read> std::fmt::Debug for EncryptedReader<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EncryptedReader")
            .field("saw_final", &self.saw_final)
            .finish_non_exhaustive()
    }
}

fn frame_prefix(final_frame: u8, cipher_len: usize) -> PortResult<[u8; 5]> {
    let mut prefix = [0_u8; 5];
    prefix[0] = final_frame;
    prefix[1..].copy_from_slice(
        &u32::try_from(cipher_len)
            .map_err(|_| invalid("backup frame is too large"))?
            .to_le_bytes(),
    );
    Ok(prefix)
}

fn frame_aad(header: &[u8], prefix: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + prefix.len());
    aad.extend_from_slice(header);
    aad.extend_from_slice(prefix);
    aad
}

fn inspect_source_tree(source: &Path, profile_root: &Path) -> PortResult<()> {
    let source = source
        .canonicalize()
        .map_err(|error| io_error("canonicalize profile data", error))?;
    ensure_within(&source, profile_root, "profile data")?;
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    for entry in WalkDir::new(&source).follow_links(false) {
        let entry = entry.map_err(|error| invalid(format!("inspect profile data: {error}")))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| io_error("inspect profile data entry", error))?;
        if is_reparse_or_symlink(&metadata) {
            return Err(invalid("profile data contains a symlink or reparse point"));
        }
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(invalid("profile data contains an unsupported file type"));
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("profile data contains too many entries"))?;
        if count > MAX_ARCHIVE_FILES {
            return Err(invalid("profile data contains too many entries"));
        }
        if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or_else(|| invalid("profile data size overflow"))?;
            if bytes > MAX_ARCHIVE_BYTES {
                return Err(invalid("profile data exceeds the backup safety limit"));
            }
        }
        let canonical = entry
            .path()
            .canonicalize()
            .map_err(|error| io_error("canonicalize profile data entry", error))?;
        ensure_within(&canonical, &source, "profile data entry")?;
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn validate_archive_entry_path(path: &Path) -> PortResult<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(tampered("backup contains an unsafe path"));
    }
    Ok(())
}

fn validate_archive_name(name: &str) -> PortResult<()> {
    if name.is_empty()
        || name.len() > 180
        || name.trim() != name
        || name.ends_with('.')
        || Path::new(name).file_name() != Some(OsStr::new(name))
        || !name.to_ascii_lowercase().ends_with(".browsbak")
        || name
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        return Err(invalid(
            "backup name must be a safe .browsbak file name without a path",
        ));
    }
    Ok(())
}

fn validate_password(password: &[u8]) -> PortResult<()> {
    if password.len() < MIN_PASSWORD_BYTES || password.len() > MAX_PASSWORD_BYTES {
        return Err(invalid(
            "backup password must be between 12 and 1024 UTF-8 bytes",
        ));
    }
    Ok(())
}

fn create_and_canonicalize(path: PathBuf, label: &'static str) -> PortResult<PathBuf> {
    fs::create_dir_all(&path)
        .map_err(|error| io_error("create managed backup directory", error))?;
    path.canonicalize()
        .map_err(|error| io_error("canonicalize managed backup directory", error))
        .and_then(|canonical| {
            if canonical.as_os_str().is_empty() {
                Err(invalid(format!("{label} is empty")))
            } else {
                Ok(canonical)
            }
        })
}

fn unique_staging_directory(parent: &Path) -> PortResult<PathBuf> {
    for _ in 0..16 {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|error| unavailable(format!("generate restore id: {error}")))?;
        let name = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = parent.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error("create restore staging directory", error)),
        }
    }
    Err(unavailable(
        "could not allocate a restore staging directory",
    ))
}

fn ensure_within(path: &Path, root: &Path, label: &'static str) -> PortResult<()> {
    if path.starts_with(root) && path != root {
        Ok(())
    } else {
        Err(invalid(format!("{label} escapes its managed root")))
    }
}

fn port_to_io(error: PortError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn io_error(context: &'static str, error: impl std::fmt::Display) -> PortError {
    PortError::new(PortErrorKind::Internal, format!("{context}: {error}"))
}

fn invalid(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::InvalidData, message)
}

fn tampered(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Tampered, message)
}

fn unavailable(message: impl Into<String>) -> PortError {
    PortError::new(PortErrorKind::Unavailable, message)
}
