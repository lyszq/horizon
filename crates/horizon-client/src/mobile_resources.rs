//! Safe transfer of prepared resources and Minecraft worlds to and from Horizon's mobile build.
//!
//! Resource archives are produced by `tools/prepare_ipad_resources.py`. Import always extracts
//! into a sibling staging directory, verifies the complete transfer manifest and pinned game
//! version, then promotes the directory with a rollback marker. A later call recovers an
//! interrupted promotion before doing new work.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const MINECRAFT_VERSION: &str = "26.3";
pub const RESOURCE_SCHEMA: u32 = 7;
pub const TRANSFER_SCHEMA: u32 = 1;
const TRANSFER_MANIFEST: &str = "horizon-transfer.json";
const RESOURCE_MANIFEST: &str = "horizon-resources.json";
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILES: usize = 250_000;
const MAX_COMPRESSION_RATIO: u64 = 300;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const JOURNAL_MARKER: &str = "horizon-mobile-import-v1\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceImportReport {
    /// Number of files covered by the transfer manifest (the manifest itself is not included).
    pub files: usize,
    /// Total uncompressed bytes covered by the transfer manifest.
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldImportReport {
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct Limits {
    archive_bytes: u64,
    total_bytes: u64,
    file_bytes: u64,
    file_count: usize,
    compression_ratio: u64,
}

const RESOURCE_LIMITS: Limits = Limits {
    archive_bytes: MAX_ARCHIVE_BYTES,
    total_bytes: MAX_TOTAL_BYTES,
    file_bytes: MAX_FILE_BYTES,
    file_count: MAX_FILES,
    compression_ratio: MAX_COMPRESSION_RATIO,
};

const WORLD_LIMITS: Limits = RESOURCE_LIMITS;

#[derive(Debug, Clone)]
struct ArchiveEntry {
    index: usize,
    name: String,
    folded: String,
    is_dir: bool,
    size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferManifest {
    schema: u32,
    minecraft: String,
    resource_schema: u32,
    files: BTreeMap<String, FileDigest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    size: u64,
    sha256: String,
}

/// Import a prepared resource archive from a path. The source archive is never modified.
pub fn import_resources_from_path(
    archive: &Path,
    destination: &Path,
) -> crate::Result<ResourceImportReport> {
    import_resources(File::open(archive)?, destination)
}

/// Import a prepared resource archive from any seekable reader.
pub fn import_resources<R: Read + Seek>(
    mut reader: R,
    destination: &Path,
) -> crate::Result<ResourceImportReport> {
    check_archive_length(&mut reader, RESOURCE_LIMITS)?;
    let mut archive = ZipArchive::new(reader)?;
    let mut report = None;
    replace_directory(destination, true, |stage| {
        report = Some(extract_resources(&mut archive, stage)?);
        Ok(())
    })?;
    report.ok_or_else(|| invalid("resource import did not produce a report").into())
}

/// Recover a resource or world import interrupted while promoting its staging directory.
///
/// Call during startup before opening resources, or before retrying an import after a cleanup
/// failure. It only acts on transaction paths owned by this module beside `destination`.
pub fn recover_import_transaction(destination: &Path) -> crate::Result<()> {
    let paths = transaction_paths(destination)?;
    recover_transaction(&paths)
}

fn extract_resources<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    stage: &Path,
) -> crate::Result<ResourceImportReport> {
    let entries = preflight(archive, RESOURCE_LIMITS)?;
    let mut found = HashMap::<String, (u64, String)>::new();
    let mut total = 0u64;
    for entry in &entries {
        if entry.is_dir {
            continue;
        }
        if (entry.folded == TRANSFER_MANIFEST.to_lowercase() && entry.name != TRANSFER_MANIFEST)
            || (entry.folded == RESOURCE_MANIFEST.to_lowercase() && entry.name != RESOURCE_MANIFEST)
            || (entry.folded == "assets/minecraft/sounds.json"
                && entry.name != "assets/minecraft/sounds.json")
        {
            return Err(
                invalid("resource archive contains a case-mismatched required path").into(),
            );
        }
        if (entry.folded == TRANSFER_MANIFEST.to_lowercase()
            || entry.folded == RESOURCE_MANIFEST.to_lowercase())
            && entry.size > MAX_MANIFEST_BYTES
        {
            return Err(invalid("resource manifest is too large").into());
        }
        let output = stage.join(Path::new(&entry.name));
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let file_limit = if entry.folded == TRANSFER_MANIFEST.to_lowercase()
            || entry.folded == RESOURCE_MANIFEST.to_lowercase()
        {
            MAX_MANIFEST_BYTES
        } else {
            RESOURCE_LIMITS.file_bytes
        };
        let (size, digest) = extract_one(archive, entry, &output, file_limit)?;
        total = total
            .checked_add(size)
            .ok_or_else(|| invalid("archive size overflow"))?;
        if total > RESOURCE_LIMITS.total_bytes {
            return Err(invalid("archive expanded beyond its total size limit").into());
        }
        found.insert(entry.folded.clone(), (size, digest));
    }

    let transfer_key = TRANSFER_MANIFEST.to_lowercase();
    let resource_key = RESOURCE_MANIFEST.to_lowercase();
    let transfer_path = stage.join(TRANSFER_MANIFEST);
    let resource_path = stage.join(RESOURCE_MANIFEST);
    let Some((transfer_size, _)) = found.get(&transfer_key) else {
        return Err(invalid("resource archive is missing its transfer manifest").into());
    };
    if *transfer_size > MAX_MANIFEST_BYTES {
        return Err(invalid("transfer manifest is too large").into());
    }
    if !found.contains_key(&resource_key) {
        return Err(invalid("resource archive is missing horizon-resources.json").into());
    }
    if !found.contains_key("assets/minecraft/sounds.json")
        || !found
            .keys()
            .any(|name| name.starts_with("assets/minecraft/sounds/") && name.ends_with(".ogg"))
    {
        return Err(invalid("resource archive is missing prepared Minecraft sounds").into());
    }
    let transfer: TransferManifest = serde_json::from_reader(File::open(&transfer_path)?)?;
    if transfer.schema != TRANSFER_SCHEMA
        || transfer.minecraft != MINECRAFT_VERSION
        || transfer.resource_schema != RESOURCE_SCHEMA
    {
        return Err(invalid("transfer manifest is not for Horizon 26.3 resources schema 7").into());
    }

    let mut listed = HashMap::with_capacity(transfer.files.len());
    for (name, record) in transfer.files {
        let normalized = safe_member_name(&name, false)?;
        let key = normalized.to_lowercase();
        if key == transfer_key || listed.contains_key(&key) {
            return Err(invalid("duplicate or reserved path in transfer manifest").into());
        }
        if record.sha256.len() != 64
            || !record.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || record.sha256.bytes().any(|byte| byte.is_ascii_uppercase())
        {
            return Err(invalid("invalid SHA-256 in transfer manifest").into());
        }
        listed.insert(key, (record.size, record.sha256));
    }
    if listed.len() + 1 != found.len() {
        return Err(invalid("transfer manifest file list does not match the archive").into());
    }
    for (name, (size, digest)) in &found {
        if name == &transfer_key {
            continue;
        }
        let Some((listed_size, listed_digest)) = listed.get(name) else {
            return Err(invalid("transfer manifest file list does not match the archive").into());
        };
        if size != listed_size || digest != listed_digest {
            return Err(invalid("resource file failed transfer size or SHA-256 validation").into());
        }
    }
    validate_resource_manifest(&resource_path)?;
    fs::remove_file(transfer_path)?;
    let transfer_size = found
        .get(&transfer_key)
        .map(|(size, _)| *size)
        .unwrap_or_default();
    Ok(ResourceImportReport {
        files: listed.len(),
        bytes: total.saturating_sub(transfer_size),
    })
}

fn validate_resource_manifest(path: &Path) -> crate::Result<()> {
    if fs::metadata(path)?.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("resource manifest is too large").into());
    }
    let manifest: serde_json::Value = serde_json::from_reader(File::open(path)?)?;
    if manifest.get("schema").and_then(serde_json::Value::as_u64)
        != Some(u64::from(RESOURCE_SCHEMA))
        || manifest
            .get("minecraft")
            .and_then(serde_json::Value::as_str)
            != Some(MINECRAFT_VERSION)
        || manifest
            .get("data_version")
            .and_then(serde_json::Value::as_i64)
            != Some(i64::from(horizon_world::DATA_VERSION))
    {
        return Err(
            invalid("prepared resources are not Horizon schema 7 for Minecraft 26.3").into(),
        );
    }
    for required in ["facts.json", "block-tags.json"] {
        if !path
            .parent()
            .unwrap_or(Path::new("."))
            .join(required)
            .is_file()
        {
            return Err(invalid("prepared resources are incomplete").into());
        }
    }
    let root = path.parent().unwrap_or(Path::new("."));
    for (manifest_key, file_name) in [
        ("facts_sha256", "facts.json"),
        ("block_tags_sha256", "block-tags.json"),
    ] {
        let expected = manifest
            .get(manifest_key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| {
                value.len() == 64
                    && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && value.bytes().all(|byte| !byte.is_ascii_uppercase())
            })
            .ok_or_else(|| invalid("prepared resource manifest has an invalid file hash"))?;
        if sha256_path(&root.join(file_name))? != expected {
            return Err(invalid("prepared resource manifest file hash does not match").into());
        }
    }
    Ok(())
}

/// Import a world backup ZIP. A single enclosing world folder is accepted and stripped; the
/// world's file bytes are kept as supplied. A stale `session.lock` is never imported.
pub fn import_world_archive<R: Read + Seek>(
    mut reader: R,
    destination: &Path,
) -> crate::Result<WorldImportReport> {
    check_archive_length(&mut reader, WORLD_LIMITS)?;
    let mut archive = ZipArchive::new(reader)?;
    let mut report = None;
    replace_directory(destination, false, |stage| {
        report = Some(extract_world(&mut archive, stage)?);
        Ok(())
    })?;
    report.ok_or_else(|| invalid("world import did not produce a report").into())
}

/// Export a world using the same single-root-folder layout as Horizon's world backups.
/// `root_name` must be a safe single path component. The source tree is read only.
pub fn export_world_archive<W: Write + Seek>(
    world: &Path,
    root_name: &str,
    output: W,
) -> crate::Result<W> {
    let root_name = safe_member_name(root_name, false)?;
    if root_name.contains('/') {
        return Err(invalid("world archive root must be one path component").into());
    }
    let metadata = fs::symlink_metadata(world)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("world source is not a real directory").into());
    }
    let mut files = Vec::new();
    let mut total = 0u64;
    collect_world_files(world, world, &mut files, &mut total)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if !files
        .iter()
        .any(|(relative, _)| relative == Path::new("level.dat"))
    {
        return Err(invalid("world source has no level.dat").into());
    }
    let mut zip = ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    for (relative, expected_size) in files {
        let name = format!(
            "{root_name}/{}",
            relative.to_string_lossy().replace('\\', "/")
        );
        safe_member_name(&name, false)?;
        zip.start_file(name, options)?;
        let mut source = File::open(world.join(&relative))?;
        let mut copied = 0u64;
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or_else(|| invalid("world file size overflow"))?;
            if copied > MAX_FILE_BYTES {
                return Err(invalid("world file changed beyond the export size limit").into());
            }
            zip.write_all(&buffer[..count])?;
        }
        if copied != expected_size {
            return Err(invalid("world file changed during export").into());
        }
    }
    Ok(zip.finish()?)
}

fn extract_world<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    stage: &Path,
) -> crate::Result<WorldImportReport> {
    let entries = preflight(archive, WORLD_LIMITS)?;
    let level_entries: Vec<_> = entries
        .iter()
        .filter(|entry| !entry.is_dir && entry.name.rsplit('/').next() == Some("level.dat"))
        .collect();
    if level_entries.len() != 1 {
        return Err(invalid("world archive must contain exactly one level.dat").into());
    }
    let level_parts: Vec<_> = level_entries[0].name.split('/').collect();
    let wrapper = match level_parts.as_slice() {
        ["level.dat"] => None,
        [one, "level.dat"] => Some(*one),
        _ => return Err(invalid("world archive has an invalid root layout").into()),
    };
    let prefix = wrapper.map(|name| format!("{name}/"));
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in &entries {
        let relative = match &prefix {
            Some(prefix) if entry.name == prefix.trim_end_matches('/') => continue,
            Some(prefix) => entry
                .name
                .strip_prefix(prefix)
                .ok_or_else(|| invalid("world archive contains files outside its root folder"))?,
            None => entry.name.as_str(),
        };
        if relative.is_empty() {
            return Err(invalid("world archive has an invalid root layout").into());
        }
        if entry.is_dir {
            continue;
        }
        if relative == "session.lock" {
            continue;
        }
        let output = stage.join(Path::new(relative));
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let (size, _) = extract_one(archive, entry, &output, WORLD_LIMITS.file_bytes)?;
        files += 1;
        bytes = bytes
            .checked_add(size)
            .ok_or_else(|| invalid("world archive size overflow"))?;
        if bytes > WORLD_LIMITS.total_bytes {
            return Err(invalid("world archive expanded beyond its total size limit").into());
        }
    }
    if files == 0 || !stage.join("level.dat").is_file() {
        return Err(invalid("world archive is missing level.dat").into());
    }
    let root = crate::save::read_dat(&stage.join("level.dat"))
        .ok_or_else(|| invalid("world archive level.dat is not valid Minecraft NBT"))?;
    if !matches!(root.get("Data"), Some(horizon_world::nbt::Tag::Compound(_))) {
        return Err(invalid("world archive level.dat has no Data compound").into());
    }
    Ok(WorldImportReport { files, bytes })
}

fn collect_world_files(
    root: &Path,
    dir: &Path,
    files: &mut Vec<(PathBuf, u64)>,
    total: &mut u64,
) -> crate::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(format!("world contains a symbolic link: {}", path.display()).into());
        }
        if kind.is_dir() {
            collect_world_files(root, &path, files, total)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root)?.to_path_buf();
            if relative == Path::new("session.lock") {
                continue;
            }
            let name = relative.to_string_lossy().replace('\\', "/");
            safe_member_name(&name, false)?;
            let size = entry.metadata()?.len();
            if size > MAX_FILE_BYTES {
                return Err(invalid("world file exceeds export size limit").into());
            }
            *total = total
                .checked_add(size)
                .ok_or_else(|| invalid("world size overflow"))?;
            if *total > MAX_TOTAL_BYTES {
                return Err(invalid("world exceeds export size limit").into());
            }
            files.push((relative, size));
            if files.len() > MAX_FILES {
                return Err(invalid("world contains too many files").into());
            }
        } else {
            return Err(format!("world contains a special file: {}", path.display()).into());
        }
    }
    Ok(())
}

fn preflight<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: Limits,
) -> crate::Result<Vec<ArchiveEntry>> {
    if archive.len() > limits.file_count {
        return Err(invalid("archive contains too many entries").into());
    }
    let mut entries = Vec::with_capacity(archive.len());
    let mut names = HashMap::<String, (String, bool)>::new();
    let mut prefixes = HashMap::<String, String>::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let name = file.name().to_owned();
        let mode_type = file.unix_mode().unwrap_or_default() & 0o170000;
        if !matches!(mode_type, 0 | 0o100000 | 0o040000) {
            return Err(invalid("archive contains a symbolic link or special file").into());
        }
        let is_dir = file.is_dir() || mode_type == 0o040000;
        let normalized = safe_member_name(&name, is_dir)?;
        let folded = normalized.to_lowercase();
        if names
            .insert(folded.clone(), (normalized.clone(), is_dir))
            .is_some()
        {
            return Err(invalid("archive contains duplicate paths").into());
        }
        let parts: Vec<_> = normalized.split('/').collect();
        for count in 1..=parts.len() {
            let prefix = parts[..count].join("/");
            let key = prefix.to_lowercase();
            if prefixes
                .get(&key)
                .is_some_and(|previous| previous != &prefix)
            {
                return Err(invalid("archive contains case-colliding paths").into());
            }
            prefixes.insert(key, prefix);
        }
        let size = file.size();
        let compressed_size = file.compressed_size();
        if is_dir {
            if size != 0 {
                return Err(invalid("archive directory entry contains data").into());
            }
        } else {
            if size > limits.file_bytes {
                return Err(invalid("archive file exceeds its size limit").into());
            }
            total = total
                .checked_add(size)
                .ok_or_else(|| invalid("archive size overflow"))?;
            if total > limits.total_bytes {
                return Err(invalid("archive exceeds total size limit").into());
            }
            if size > 16 * 1024 * 1024
                && (compressed_size == 0
                    || u128::from(size)
                        > u128::from(compressed_size) * u128::from(limits.compression_ratio))
            {
                return Err(invalid("archive contains a suspicious compression ratio").into());
            }
        }
        entries.push(ArchiveEntry {
            index,
            name: normalized,
            folded,
            is_dir,
            size,
        });
    }
    for entry in &entries {
        let parts: Vec<_> = entry.folded.split('/').collect();
        for count in 1..parts.len() {
            let parent = parts[..count].join("/");
            if names.get(&parent).is_some_and(|(_, is_dir)| !is_dir) {
                return Err(invalid("archive file is also used as a parent directory").into());
            }
        }
    }
    Ok(entries)
}

fn safe_member_name(name: &str, is_dir: bool) -> io::Result<String> {
    if name.is_empty() || name.len() > 4096 || name.starts_with('/') || name.contains('\\') {
        return Err(invalid("archive contains an unsafe path"));
    }
    let path = if is_dir {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    if path.is_empty() || path.contains('\0') {
        return Err(invalid("archive contains an unsafe path"));
    }
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts {
        if part.is_empty()
            || *part == "."
            || *part == ".."
            || part.ends_with('.')
            || part.ends_with(' ')
            || part
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(invalid("archive contains an unsafe path component"));
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
        ) || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(invalid("archive contains a Windows device path"));
        }
    }
    Ok(parts.join("/"))
}

fn extract_one<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    entry: &ArchiveEntry,
    output: &Path,
    expanded_limit: u64,
) -> crate::Result<(u64, String)> {
    if entry.size > expanded_limit {
        return Err(invalid("archive file exceeds its size limit").into());
    }
    let mut input = archive.by_index(entry.index)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut hash = Sha256::new();
    let mut size = 0u64;
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| invalid("archive file size overflow"))?;
        if size > entry.size || size > expanded_limit {
            return Err(invalid("archive expanded beyond its declared size").into());
        }
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if size != entry.size {
        return Err(invalid("archive file size does not match its directory entry").into());
    }
    output.sync_all()?;
    let digest = hex_lower(&hash.finalize());
    Ok((size, digest))
}

fn check_archive_length<R: Seek>(reader: &mut R, limits: Limits) -> crate::Result<()> {
    let start = reader.stream_position()?;
    let end = reader.seek(SeekFrom::End(0))?;
    let size = end.saturating_sub(start);
    reader.seek(SeekFrom::Start(start))?;
    if size > limits.archive_bytes {
        return Err(invalid("archive exceeds compressed size limit").into());
    }
    Ok(())
}

struct TransactionPaths {
    destination: PathBuf,
    stage: PathBuf,
    backup: PathBuf,
    journal: PathBuf,
}

fn transaction_paths(destination: &Path) -> crate::Result<TransactionPaths> {
    let raw_parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(raw_parent)?;
    let parent = fs::canonicalize(raw_parent)?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| invalid("destination needs a normal file name"))?;
    let destination = parent.join(name);
    let stem = format!(".{name}.horizon-mobile");
    Ok(TransactionPaths {
        destination,
        stage: parent.join(format!("{stem}.stage")),
        backup: parent.join(format!("{stem}.backup")),
        journal: parent.join(format!("{stem}.transaction")),
    })
}

fn replace_directory(
    destination: &Path,
    allow_replace: bool,
    populate: impl FnOnce(&Path) -> crate::Result<()>,
) -> crate::Result<()> {
    let paths = transaction_paths(destination)?;
    // A marker can belong to a live import in another thread. Only the app's startup recovery
    // path may interpret it as interrupted; an import must never delete another import's stage.
    if path_exists(&paths.journal)? {
        return Err(
            invalid("an import transaction is unfinished; recover it before importing").into(),
        );
    }
    if path_exists(&paths.stage)? || path_exists(&paths.backup)? {
        return Err(invalid("stale import transaction paths need recovery").into());
    }
    if path_exists(&paths.destination)? {
        ensure_real_directory(&paths.destination)?;
        if !allow_replace {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "world import destination already exists",
            )
            .into());
        }
    }
    let mut journal = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&paths.journal)?;
    if let Err(error) = journal
        .write_all(JOURNAL_MARKER.as_bytes())
        .and_then(|()| journal.sync_all())
    {
        drop(journal);
        let _ = fs::remove_file(&paths.journal);
        return Err(error.into());
    }
    drop(journal);
    sync_parent(&paths.destination);
    fs::create_dir(&paths.stage)?;

    if let Err(error) = populate(&paths.stage) {
        let cleanup =
            fs::remove_dir_all(&paths.stage).and_then(|_| fs::remove_file(&paths.journal));
        if let Err(cleanup_error) = cleanup {
            return Err(format!("{error}; import cleanup needs recovery: {cleanup_error}").into());
        }
        return Err(error);
    }
    sync_tree_directories(&paths.stage)?;
    sync_parent(&paths.stage);
    let had_destination = path_exists(&paths.destination)?;
    if had_destination {
        if let Err(error) = fs::rename(&paths.destination, &paths.backup) {
            if let Err(cleanup) = remove_stage_and_journal(&paths) {
                return Err(format!("{error}; import cleanup needs recovery: {cleanup}").into());
            }
            return Err(error.into());
        }
        sync_parent(&paths.destination);
    }
    if let Err(error) = fs::rename(&paths.stage, &paths.destination) {
        if had_destination {
            if let Err(rollback) = fs::rename(&paths.backup, &paths.destination) {
                return Err(format!(
                    "could not promote imported data ({error}) or restore the previous directory \
                     ({rollback}); recovery is pending"
                )
                .into());
            }
        }
        if let Err(cleanup) = remove_stage_and_journal(&paths) {
            return Err(format!("{error}; import cleanup needs recovery: {cleanup}").into());
        }
        sync_parent(&paths.destination);
        return Err(error.into());
    }
    sync_parent(&paths.destination);
    // The commit point is the staging rename. Cleanup is recoverable and cannot invalidate the
    // now-active, fully verified directory.
    match path_exists(&paths.backup) {
        Ok(true) if fs::remove_dir_all(&paths.backup).is_ok() => {
            let _ = fs::remove_file(&paths.journal);
        }
        Ok(false) => {
            let _ = fs::remove_file(&paths.journal);
        }
        _ => {}
    }
    sync_parent(&paths.destination);
    Ok(())
}

fn recover_transaction(paths: &TransactionPaths) -> crate::Result<()> {
    if !path_exists(&paths.journal)? {
        return Ok(());
    }
    let journal_metadata = fs::symlink_metadata(&paths.journal)?;
    if !journal_metadata.is_file() || journal_metadata.file_type().is_symlink() {
        return Err(invalid("import transaction journal is not a regular file").into());
    }
    if journal_metadata.len() != JOURNAL_MARKER.len() as u64 {
        return Err(invalid(
            "import transaction journal is corrupt; active data was left untouched",
        )
        .into());
    }
    let mut marker = String::new();
    File::open(&paths.journal)?.read_to_string(&mut marker)?;
    if marker != JOURNAL_MARKER {
        return Err(invalid(
            "import transaction journal is corrupt; active data was left untouched",
        )
        .into());
    }
    for path in [&paths.destination, &paths.stage, &paths.backup] {
        if path_exists(path)? {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("import transaction path is a symbolic link").into());
            }
            if !metadata.is_dir() {
                return Err(invalid("import transaction path is not a directory").into());
            }
        }
    }
    let active = path_exists(&paths.destination)?;
    let stage = path_exists(&paths.stage)?;
    let backup = path_exists(&paths.backup)?;
    match (active, stage, backup) {
        (true, true, false) => fs::remove_dir_all(&paths.stage)?,
        (true, false, true) => fs::remove_dir_all(&paths.backup)?,
        (true, true, true) => {
            return Err(invalid("ambiguous import state; active data was left untouched").into());
        }
        (false, true, true) => {
            fs::rename(&paths.backup, &paths.destination)?;
            fs::remove_dir_all(&paths.stage)?;
        }
        (false, false, true) => fs::rename(&paths.backup, &paths.destination)?,
        (false, true, false) => fs::remove_dir_all(&paths.stage)?,
        // This state is reached after the new directory has committed and the old backup was
        // already removed, but the process stopped before deleting the journal marker.
        (true, false, false) | (false, false, false) => {}
    }
    fs::remove_file(&paths.journal)?;
    sync_parent(&paths.destination);
    Ok(())
}

fn remove_stage_and_journal(paths: &TransactionPaths) -> io::Result<()> {
    if path_exists(&paths.stage)? {
        fs::remove_dir_all(&paths.stage)?;
    }
    fs::remove_file(&paths.journal)
}

fn ensure_real_directory(path: &Path) -> crate::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("import destination is not a real directory").into());
    }
    Ok(())
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

fn sync_tree_directories(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree_directories(&entry.path())?;
        }
    }
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[usize::from(byte >> 4)] as char);
        output.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn sha256_path(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut hash = Sha256::new();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex_lower(&hash.finalize()))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Cursor,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(".local")
            .join(format!(
                "horizon-mobile-{label}-{}-{nonce}",
                std::process::id()
            ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn resource_zip(bad_hash: bool, extra_path: Option<&str>) -> Vec<u8> {
        let mut files = BTreeMap::<String, Vec<u8>>::new();
        files.insert(
            RESOURCE_MANIFEST.into(),
            format!(
                r#"{{"schema":7,"minecraft":"26.3","data_version":{},"facts_sha256":"{}","block_tags_sha256":"{}","resource_files":3}}"#,
                horizon_world::DATA_VERSION,
                hex_lower(&Sha256::digest(b"{}")),
                hex_lower(&Sha256::digest(b"{}")),
            )
            .into_bytes(),
        );
        files.insert("facts.json".into(), b"{}".to_vec());
        files.insert("block-tags.json".into(), b"{}".to_vec());
        files.insert("assets/minecraft/sounds.json".into(), b"{}".to_vec());
        files.insert(
            "assets/minecraft/sounds/test.ogg".into(),
            b"synthetic audio".to_vec(),
        );
        let records: BTreeMap<_, _> = files
            .iter()
            .map(|(name, data)| {
                let hash = Sha256::digest(data);
                (
                    name.clone(),
                    format!(
                        "{{\"size\":{},\"sha256\":\"{}\"}}",
                        data.len(),
                        hex_lower(&hash)
                    ),
                )
            })
            .collect();
        let mut manifest =
            format!("{{\"schema\":1,\"minecraft\":\"26.3\",\"resource_schema\":7,\"files\":{{");
        for (index, (name, record)) in records.iter().enumerate() {
            if index > 0 {
                manifest.push(',');
            }
            let record = if bad_hash && name == "facts.json" {
                format!("{{\"size\":2,\"sha256\":\"{}\"}}", "0".repeat(64))
            } else {
                record.clone()
            };
            manifest.push_str(&format!("\"{name}\":{record}"));
        }
        manifest.push_str("}}");
        files.insert(TRANSFER_MANIFEST.into(), manifest.into_bytes());
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in files {
            zip.start_file(name, options).unwrap();
            zip.write_all(&data).unwrap();
        }
        if let Some(name) = extra_path {
            zip.start_file(name, options).unwrap();
            zip.write_all(b"bad").unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn resource_import_verifies_hashes_and_replaces_as_one_directory() {
        let root = temp_dir("resources");
        let destination = root.join("resources");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("old.txt"), b"old resources").unwrap();

        let report =
            import_resources(Cursor::new(resource_zip(false, None)), &destination).unwrap();
        assert_eq!(report.files, 5);
        assert_eq!(fs::read(destination.join("facts.json")).unwrap(), b"{}");
        assert!(!destination.join("old.txt").exists());
        assert!(!destination.join(TRANSFER_MANIFEST).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_validation_keeps_the_previous_resources_directory_intact() {
        let root = temp_dir("rollback");
        let destination = root.join("resources");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("marker"), b"keep me").unwrap();

        assert!(import_resources(Cursor::new(resource_zip(true, None)), &destination).is_err());
        assert_eq!(fs::read(destination.join("marker")).unwrap(), b"keep me");
        assert!(!root.join(".resources.horizon-mobile.stage").exists());
        assert!(!root.join(".resources.horizon-mobile.backup").exists());
        assert!(!root.join(".resources.horizon-mobile.transaction").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_preflight_failure_keeps_active_resources_and_cleans_its_stage() {
        let root = temp_dir("preflight-failure");
        let destination = root.join("resources");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("marker"), b"keep").unwrap();

        assert!(
            import_resources(
                Cursor::new(resource_zip(false, Some("../escape"))),
                &destination
            )
            .is_err()
        );
        assert_eq!(fs::read(destination.join("marker")).unwrap(), b"keep");
        assert!(!root.join("escape").exists());
        assert!(!root.join(".resources.horizon-mobile.stage").exists());
        assert!(!root.join(".resources.horizon-mobile.transaction").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_paths_reject_traversal_windows_paths_and_device_names() {
        for name in [
            "../outside",
            "/absolute",
            "C:/outside",
            "dir\\file",
            "NUL.txt",
        ] {
            assert!(safe_member_name(name, false).is_err(), "accepted {name}");
        }
    }

    #[test]
    fn recovery_restores_backup_when_promotion_was_interrupted() {
        let root = temp_dir("recovery");
        let destination = root.join("resources");
        let paths = transaction_paths(&destination).unwrap();
        fs::create_dir(&paths.backup).unwrap();
        fs::write(paths.backup.join("old"), b"previous").unwrap();
        fs::create_dir(&paths.stage).unwrap();
        fs::write(paths.stage.join("new"), b"candidate").unwrap();
        fs::write(&paths.journal, JOURNAL_MARKER).unwrap();

        recover_transaction(&paths).unwrap();
        assert_eq!(fs::read(destination.join("old")).unwrap(), b"previous");
        assert!(!destination.join("new").exists());
        assert!(!paths.journal.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_handles_every_unambiguous_transaction_state_and_preserves_active_data() {
        for (active, stage, backup) in [
            (true, false, false),
            (true, true, false),
            (true, false, true),
            (false, true, true),
            (false, false, true),
            (false, true, false),
            (false, false, false),
        ] {
            let root = temp_dir("recovery-matrix");
            let paths = transaction_paths(&root.join("resources")).unwrap();
            if active {
                fs::create_dir(&paths.destination).unwrap();
                fs::write(paths.destination.join("active"), b"active").unwrap();
            }
            if stage {
                fs::create_dir(&paths.stage).unwrap();
                fs::write(paths.stage.join("staged"), b"candidate").unwrap();
            }
            if backup {
                fs::create_dir(&paths.backup).unwrap();
                fs::write(paths.backup.join("backup"), b"previous").unwrap();
            }
            fs::write(&paths.journal, JOURNAL_MARKER).unwrap();

            recover_transaction(&paths).unwrap();

            assert!(
                !path_exists(&paths.journal).unwrap(),
                "journal state {active}/{stage}/{backup}"
            );
            let destination_exists = path_exists(&paths.destination).unwrap();
            let stage_exists = path_exists(&paths.stage).unwrap();
            let backup_exists = path_exists(&paths.backup).unwrap();
            assert!(!stage_exists, "stage state {active}/{stage}/{backup}");
            assert!(!backup_exists, "backup state {active}/{stage}/{backup}");
            if active || backup {
                assert!(
                    destination_exists,
                    "destination state {active}/{stage}/{backup}"
                );
                if active {
                    assert_eq!(
                        fs::read(paths.destination.join("active")).unwrap(),
                        b"active"
                    );
                } else {
                    assert_eq!(
                        fs::read(paths.destination.join("backup")).unwrap(),
                        b"previous"
                    );
                }
            } else {
                assert!(
                    !destination_exists,
                    "destination state {active}/{stage}/{backup}"
                );
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn recovery_leaves_ambiguous_state_untouched() {
        let root = temp_dir("recovery-ambiguous");
        let paths = transaction_paths(&root.join("resources")).unwrap();
        for (directory, marker) in [
            (&paths.destination, "active"),
            (&paths.stage, "staged"),
            (&paths.backup, "backup"),
        ] {
            fs::create_dir(directory).unwrap();
            fs::write(directory.join(marker), marker).unwrap();
        }
        fs::write(&paths.journal, JOURNAL_MARKER).unwrap();

        assert!(recover_transaction(&paths).is_err());
        assert_eq!(
            fs::read(paths.destination.join("active")).unwrap(),
            b"active"
        );
        assert_eq!(fs::read(paths.stage.join("staged")).unwrap(), b"staged");
        assert_eq!(fs::read(paths.backup.join("backup")).unwrap(), b"backup");
        assert_eq!(fs::read_to_string(&paths.journal).unwrap(), JOURNAL_MARKER);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_rejects_corrupt_journal_without_touching_active_resources() {
        let root = temp_dir("recovery-corrupt");
        let paths = transaction_paths(&root.join("resources")).unwrap();
        fs::create_dir(&paths.destination).unwrap();
        fs::write(paths.destination.join("active"), b"keep").unwrap();
        fs::create_dir(&paths.stage).unwrap();
        fs::write(paths.stage.join("staged"), b"candidate").unwrap();
        fs::write(&paths.journal, b"unexpectedly large invalid journal").unwrap();

        assert!(recover_transaction(&paths).is_err());
        assert_eq!(fs::read(paths.destination.join("active")).unwrap(), b"keep");
        assert_eq!(fs::read(paths.stage.join("staged")).unwrap(), b"candidate");
        assert!(paths.journal.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn new_import_does_not_take_over_a_transaction_with_a_live_marker() {
        let root = temp_dir("live-transaction");
        let destination = root.join("resources");
        let paths = transaction_paths(&destination).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("active"), b"keep").unwrap();
        fs::create_dir(&paths.stage).unwrap();
        fs::write(paths.stage.join("in-progress"), b"another import").unwrap();
        fs::write(&paths.journal, JOURNAL_MARKER).unwrap();

        assert!(import_resources(Cursor::new(resource_zip(false, None)), &destination).is_err());
        assert_eq!(fs::read(destination.join("active")).unwrap(), b"keep");
        assert_eq!(
            fs::read(paths.stage.join("in-progress")).unwrap(),
            b"another import"
        );
        assert_eq!(fs::read_to_string(&paths.journal).unwrap(), JOURNAL_MARKER);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zip_preflight_enforces_declared_file_count_and_expansion_limits() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, bytes) in [("a", b"123".as_slice()), ("b", b"456".as_slice())] {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let limits = Limits {
            archive_bytes: 128,
            total_bytes: 6,
            file_bytes: 3,
            file_count: 2,
            compression_ratio: 300,
        };
        let mut archive = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(preflight(&mut archive, limits).unwrap().len(), 2);

        let mut too_many = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        assert!(
            preflight(
                &mut too_many,
                Limits {
                    file_count: 1,
                    ..limits
                }
            )
            .is_err()
        );
        let mut too_large = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
        assert!(
            preflight(
                &mut too_large,
                Limits {
                    file_bytes: 2,
                    ..limits
                }
            )
            .is_err()
        );
        let mut too_much_total = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert!(
            preflight(
                &mut too_much_total,
                Limits {
                    total_bytes: 5,
                    ..limits
                }
            )
            .is_err()
        );
    }

    #[test]
    fn zip_preflight_rejects_symlinks_duplicate_names_and_case_collisions() {
        let mut symlink_zip = ZipWriter::new(Cursor::new(Vec::new()));
        symlink_zip
            .start_file("link", SimpleFileOptions::default())
            .unwrap();
        symlink_zip.write_all(b"target").unwrap();
        let mut symlink_bytes = symlink_zip.finish().unwrap().into_inner();
        let central = symlink_bytes
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        symlink_bytes[central + 5] = 3; // Unix creator OS in version-made-by.
        symlink_bytes[central + 38..central + 42]
            .copy_from_slice(&((0o120777u32) << 16).to_le_bytes());
        let mut symlink_archive = ZipArchive::new(Cursor::new(symlink_bytes)).unwrap();
        assert!(
            preflight(&mut symlink_archive, RESOURCE_LIMITS)
                .unwrap_err()
                .to_string()
                .contains("symbolic link")
        );

        let mut duplicate_zip = ZipWriter::new(Cursor::new(Vec::new()));
        for _ in 0..2 {
            duplicate_zip
                .start_file("same", SimpleFileOptions::default())
                .unwrap();
            duplicate_zip.write_all(b"payload").unwrap();
        }
        let mut duplicate_archive =
            ZipArchive::new(Cursor::new(duplicate_zip.finish().unwrap().into_inner())).unwrap();
        assert!(
            preflight(&mut duplicate_archive, RESOURCE_LIMITS)
                .unwrap_err()
                .to_string()
                .contains("duplicate paths")
        );

        let mut case_zip = ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["Assets/file", "assets/other"] {
            case_zip
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            case_zip.write_all(b"payload").unwrap();
        }
        let mut case_archive =
            ZipArchive::new(Cursor::new(case_zip.finish().unwrap().into_inner())).unwrap();
        assert!(
            preflight(&mut case_archive, RESOURCE_LIMITS)
                .unwrap_err()
                .to_string()
                .contains("case-colliding")
        );
    }

    #[test]
    fn zip_preflight_rejects_high_expansion_before_extracting() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            "large-zero-file.bin",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&vec![0u8; 17 * 1024 * 1024]).unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert!(
            preflight(&mut archive, RESOURCE_LIMITS)
                .unwrap_err()
                .to_string()
                .contains("compression ratio")
        );
    }

    #[test]
    fn world_export_and_import_preserve_files_and_drop_session_lock() {
        let root = temp_dir("world");
        let source = root.join("source");
        fs::create_dir_all(source.join("region")).unwrap();
        crate::save::write_dat(
            &source.join("level.dat"),
            horizon_world::nbt::Tag::compound([(
                "Data",
                horizon_world::nbt::Tag::compound([(
                    "DataVersion",
                    horizon_world::nbt::Tag::Int(horizon_world::DATA_VERSION),
                )]),
            )]),
        )
        .unwrap();
        fs::write(source.join("region/r.0.0.mca"), b"region bytes").unwrap();
        fs::write(source.join("session.lock"), b"active lock").unwrap();

        let archive = export_world_archive(&source, "My World", Cursor::new(Vec::new()))
            .unwrap()
            .into_inner();
        let destination = root.join("imported");
        let report = import_world_archive(Cursor::new(archive), &destination).unwrap();
        assert_eq!(report.files, 2);
        assert_eq!(
            fs::read(destination.join("region/r.0.0.mca")).unwrap(),
            b"region bytes"
        );
        assert!(!destination.join("session.lock").exists());
        assert!(crate::save::read_dat(&destination.join("level.dat")).is_some());
        fs::remove_dir_all(root).unwrap();
    }
}
