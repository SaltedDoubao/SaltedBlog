//! 备份 / 恢复：签名 zip 格式（manifest + blog.db/blog.dump + uploads/）

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use fs2::available_space;
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbEngine {
    Sqlite,
    Postgres,
}

impl DbEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub created_at: String,
    pub engines: Vec<String>,
    pub app: String,
    pub file_hashes: BTreeMap<String, String>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupItem {
    pub name: String,
    pub size_bytes: u64,
    pub created_at: String,
    pub engine: Option<String>,
}

pub fn detect_engine(database_url: &str) -> ApiResult<DbEngine> {
    let lower = database_url.to_ascii_lowercase();
    if lower.starts_with("sqlite:") {
        Ok(DbEngine::Sqlite)
    } else if lower.starts_with("postgres:") || lower.starts_with("postgresql:") {
        Ok(DbEngine::Postgres)
    } else {
        Err(ApiError::bad_request("unsupported DATABASE_URL engine"))
    }
}

pub fn sqlite_path_from_url(database_url: &str) -> ApiResult<PathBuf> {
    let rest = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))
        .ok_or_else(|| ApiError::bad_request("invalid sqlite DATABASE_URL"))?;
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path == ":memory:" {
        return Err(ApiError::bad_request("cannot backup in-memory sqlite"));
    }
    Ok(PathBuf::from(path))
}

pub fn is_valid_backup_name(name: &str) -> bool {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return false;
    }
    let Some(stem) = name.strip_suffix(".zip") else {
        return false;
    };
    let parts: Vec<&str> = stem.split('_').collect();
    // saltedblog_{engine}_{YYYYMMDD}_{HHMMSS}
    if parts.len() != 4 || parts[0] != "saltedblog" {
        return false;
    }
    if parts[1] != "sqlite" && parts[1] != "postgres" {
        return false;
    }
    parts[2].len() == 8
        && parts[2].chars().all(|c| c.is_ascii_digit())
        && parts[3].len() == 6
        && parts[3].chars().all(|c| c.is_ascii_digit())
}

pub fn backup_path(cfg: &Config, name: &str) -> ApiResult<PathBuf> {
    if !is_valid_backup_name(name) {
        return Err(ApiError::bad_request("invalid backup filename"));
    }
    Ok(cfg.backup_dir.join(name))
}

fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn ensure_disk_space(cfg: &Config, engine: DbEngine) -> ApiResult<()> {
    let upload_bytes = dir_size(&cfg.upload_dir);
    let db_bytes = match engine {
        DbEngine::Sqlite => {
            let p = sqlite_path_from_url(&cfg.database_url)?;
            fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
        }
        DbEngine::Postgres => upload_bytes / 4 + 16 * 1024 * 1024, // 粗估
    };
    let need = db_bytes
        .saturating_add(upload_bytes)
        .saturating_add(32 * 1024 * 1024);
    let avail = available_space(&cfg.backup_dir)
        .map_err(|e| ApiError::internal(format!("failed to check disk space: {e}")))?;
    if avail < need {
        return Err(ApiError::bad_request(format!(
            "磁盘空间不足：需要约 {} MB，可用 {} MB",
            need / (1024 * 1024),
            avail / (1024 * 1024)
        )));
    }
    Ok(())
}

fn zip_options_stored() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

fn zip_options_deflated() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

fn add_file_to_zip(
    zip: &mut ZipWriter<File>,
    name: &str,
    path: &Path,
    stored: bool,
) -> ApiResult<()> {
    let opts = if stored {
        zip_options_stored()
    } else {
        zip_options_deflated()
    };
    zip.start_file(name, opts)
        .map_err(|e| ApiError::internal(format!("zip start_file: {e}")))?;
    let mut f = File::open(path).map_err(|e| ApiError::internal(format!("open {path:?}: {e}")))?;
    io::copy(&mut f, zip).map_err(|e| ApiError::internal(format!("zip copy: {e}")))?;
    Ok(())
}

fn add_dir_to_zip(zip: &mut ZipWriter<File>, prefix: &str, dir: &Path) -> ApiResult<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let rel = path.strip_prefix(dir).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() || rel_str == "." {
            continue;
        }
        let name = format!("{prefix}/{rel_str}");
        if entry.file_type().is_dir() {
            let dir_name = if name.ends_with('/') {
                name
            } else {
                format!("{name}/")
            };
            zip.add_directory(dir_name, zip_options_stored())
                .map_err(|e| ApiError::internal(format!("zip add_directory: {e}")))?;
        } else if entry.file_type().is_file() {
            // 图片等已压缩内容用 Stored，降低 CPU
            add_file_to_zip(zip, &name, path, true)?;
        }
    }
    Ok(())
}

async fn dump_sqlite(db: &DatabaseConnection, dest: &Path) -> ApiResult<()> {
    let path_sql = dest
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "''");
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    db.execute_unprepared(&format!("VACUUM INTO '{path_sql}'"))
        .await
        .map_err(|e| ApiError::internal(format!("VACUUM INTO failed: {e}")))?;
    Ok(())
}

fn dump_postgres(database_url: &str, dest: &Path) -> ApiResult<()> {
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    let parsed =
        url::Url::parse(database_url).map_err(|_| ApiError::internal("invalid postgres URL"))?;
    let db_name = parsed.path().trim_start_matches('/');
    let mut command = Command::new("pg_dump");
    command
        .arg("--host")
        .arg(parsed.host_str().unwrap_or("postgres"))
        .arg("--port")
        .arg(parsed.port_or_known_default().unwrap_or(5432).to_string())
        .arg("--username")
        .arg(parsed.username())
        .arg("--dbname")
        .arg(db_name)
        .arg("--format=custom")
        .arg("--no-owner")
        .arg("--no-acl")
        .arg("--file")
        .arg(dest);
    if let Some(password) = parsed.password() {
        command.env("PGPASSWORD", password);
    }
    let output = command.output().map_err(|e| {
        ApiError::internal(format!(
            "failed to run pg_dump (is postgresql-client installed?): {e}"
        ))
    })?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(ApiError::internal(format!("pg_dump failed: {err}")));
    }
    Ok(())
}

fn restore_postgres(database_url: &str, dump_path: &Path) -> ApiResult<()> {
    let parsed =
        url::Url::parse(database_url).map_err(|_| ApiError::internal("invalid postgres URL"))?;
    let db_name = parsed.path().trim_start_matches('/');
    let mut command = Command::new("pg_restore");
    command
        .arg("--host")
        .arg(parsed.host_str().unwrap_or("postgres"))
        .arg("--port")
        .arg(parsed.port_or_known_default().unwrap_or(5432).to_string())
        .arg("--username")
        .arg(parsed.username())
        .arg("--dbname")
        .arg(db_name)
        .arg("--clean")
        .arg("--if-exists")
        .arg("--no-owner")
        .arg("--no-acl")
        .arg("--single-transaction")
        .arg("--exit-on-error")
        .arg(dump_path);
    if let Some(password) = parsed.password() {
        command.env("PGPASSWORD", password);
    }
    let import = command
        .output()
        .map_err(|e| ApiError::internal(format!("failed to run pg_restore: {e}")))?;
    if !import.status.success() {
        let err = String::from_utf8_lossy(&import.stderr);
        return Err(ApiError::internal(format!("pg_restore failed: {err}")));
    }
    Ok(())
}

async fn verify_sqlite_integrity(path: &Path) -> ApiResult<()> {
    let url = format!("sqlite://{}?mode=ro", path.display());
    let db = Database::connect(&url)
        .await
        .map_err(|e| ApiError::bad_request(format!("cannot open restored sqlite database: {e}")))?;
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "PRAGMA integrity_check".to_string(),
        ))
        .await
        .map_err(|e| ApiError::bad_request(format!("sqlite integrity check failed: {e}")))?
        .ok_or_else(|| ApiError::bad_request("sqlite integrity check returned no result"))?;
    let result: String = row
        .try_get_by_index(0)
        .map_err(|e| ApiError::bad_request(format!("sqlite integrity result invalid: {e}")))?;
    db.close()
        .await
        .map_err(|e| ApiError::internal(format!("close integrity database: {e}")))?;
    if result != "ok" {
        return Err(ApiError::bad_request(format!(
            "sqlite integrity check rejected backup: {}",
            result.chars().take(200).collect::<String>()
        )));
    }
    Ok(())
}

fn clear_dir_contents(dir: &Path) -> ApiResult<()> {
    if !dir.exists() {
        fs::create_dir_all(dir).map_err(|e| ApiError::internal(e.to_string()))?;
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|e| ApiError::internal(e.to_string()))? {
        let entry = entry.map_err(|e| ApiError::internal(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| ApiError::internal(e.to_string()))?;
        } else {
            fs::remove_file(&path).map_err(|e| ApiError::internal(e.to_string()))?;
        }
    }
    Ok(())
}

fn safe_zip_path(name: &str) -> ApiResult<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(ApiError::bad_request("zip entry has absolute path"));
    }
    for c in path.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(ApiError::bad_request("zip entry has illegal path")),
        }
    }
    Ok(path.to_path_buf())
}

fn hash_reader(mut reader: impl Read) -> ApiResult<String> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| ApiError::bad_request(format!("read backup entry: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn hash_file(path: &Path) -> ApiResult<String> {
    let file = File::open(path).map_err(|e| ApiError::internal(format!("hash file: {e}")))?;
    hash_reader(file)
}

fn signing_key(cfg: &Config) -> &[u8] {
    if cfg.backup_signing_key.is_empty() {
        b"saltedblog-development-backup-key"
    } else {
        cfg.backup_signing_key.as_bytes()
    }
}

fn manifest_payload(manifest: &Manifest) -> ApiResult<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "format_version": manifest.format_version,
        "created_at": manifest.created_at,
        "engines": manifest.engines,
        "app": manifest.app,
        "file_hashes": manifest.file_hashes,
    }))
    .map_err(|e| ApiError::internal(format!("manifest payload: {e}")))
}

fn sign_manifest(cfg: &Config, manifest: &Manifest) -> ApiResult<String> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(signing_key(cfg))
        .map_err(|_| ApiError::internal("invalid backup signing key"))?;
    mac.update(&manifest_payload(manifest)?);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn verify_manifest(cfg: &Config, manifest: &Manifest) -> ApiResult<()> {
    let signature = hex::decode(&manifest.signature)
        .map_err(|_| ApiError::bad_request("invalid backup signature"))?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(signing_key(cfg))
        .map_err(|_| ApiError::internal("invalid backup signing key"))?;
    mac.update(&manifest_payload(manifest)?);
    mac.verify_slice(&signature)
        .map_err(|_| ApiError::bad_request("backup signature verification failed"))
}

fn collect_backup_hashes(
    work_dir: &Path,
    upload_dir: &Path,
    engine: DbEngine,
) -> ApiResult<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    let db_name = match engine {
        DbEngine::Sqlite => "blog.db",
        DbEngine::Postgres => "blog.dump",
    };
    hashes.insert(db_name.to_string(), hash_file(&work_dir.join(db_name))?);
    if upload_dir.exists() {
        for entry in WalkDir::new(upload_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let rel = entry
                .path()
                .strip_prefix(upload_dir)
                .map_err(|_| ApiError::internal("upload path"))?;
            let name = format!("uploads/{}", rel.to_string_lossy().replace('\\', "/"));
            hashes.insert(name, hash_file(entry.path())?);
        }
    }
    Ok(hashes)
}

fn extract_uploads_from_zip(archive_path: &Path, upload_dir: &Path) -> ApiResult<()> {
    let file = File::open(archive_path).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| ApiError::bad_request(format!("invalid zip: {e}")))?;

    clear_dir_contents(upload_dir)?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| ApiError::internal(format!("zip entry: {e}")))?;
        let raw_name = entry.name().to_string();
        let Some(rel) = raw_name
            .strip_prefix("uploads/")
            .or_else(|| raw_name.strip_prefix("uploads\\"))
        else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let rel_path = safe_zip_path(rel)?;
        let out_path = upload_dir.join(&rel_path);
        if raw_name.ends_with('/') || entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| ApiError::internal(e.to_string()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
        }
        let mut out = File::create(&out_path).map_err(|e| ApiError::internal(e.to_string()))?;
        io::copy(&mut entry, &mut out).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    Ok(())
}

fn swap_upload_dirs(staged: &Path, upload_dir: &Path, stamp: &str) -> ApiResult<()> {
    let parent = upload_dir
        .parent()
        .ok_or_else(|| ApiError::internal("upload directory has no parent"))?;
    fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
    let previous = parent.join(format!(".uploads_previous_{stamp}"));
    let _ = fs::remove_dir_all(&previous);
    if upload_dir.exists() {
        fs::rename(upload_dir, &previous).map_err(|e| {
            ApiError::internal(format!("cannot stage current upload directory: {e}"))
        })?;
    }
    if let Err(err) = fs::rename(staged, upload_dir) {
        if previous.exists() {
            let _ = fs::rename(&previous, upload_dir);
        }
        return Err(ApiError::internal(format!(
            "cannot atomically activate restored uploads: {err}"
        )));
    }
    let _ = fs::remove_dir_all(previous);
    Ok(())
}

fn read_manifest(cfg: &Config, archive_path: &Path) -> ApiResult<(Manifest, bool, bool)> {
    let file = File::open(archive_path).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| ApiError::bad_request(format!("invalid zip: {e}")))?;

    let has_db = zip.by_name("blog.db").is_ok();
    let has_dump = zip.by_name("blog.dump").is_ok();

    let mut entry = zip
        .by_name("manifest.json")
        .map_err(|_| ApiError::bad_request("备份包缺少 manifest.json"))?;
    let mut buf = String::new();
    entry
        .read_to_string(&mut buf)
        .map_err(|e| ApiError::bad_request(format!("read manifest: {e}")))?;
    drop(entry);
    let manifest: Manifest = serde_json::from_str(&buf)
        .map_err(|e| ApiError::bad_request(format!("invalid manifest.json: {e}")))?;
    if manifest.format_version != FORMAT_VERSION {
        return Err(ApiError::bad_request(format!(
            "unsupported backup format_version: {}",
            manifest.format_version
        )));
    }
    verify_manifest(cfg, &manifest)?;
    if zip.len() > 50_000 {
        return Err(ApiError::bad_request("backup contains too many entries"));
    }
    let max_unpacked = (cfg.backup_upload_max_mb as u64)
        .saturating_mul(4)
        .max(1024)
        * 1024
        * 1024;
    let mut total = 0u64;
    let mut seen = HashSet::new();
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| ApiError::bad_request(format!("zip entry: {e}")))?;
        let name = entry.name().replace('\\', "/");
        if name == "manifest.json" || entry.is_dir() {
            continue;
        }
        if name != "blog.db" && name != "blog.dump" && !name.starts_with("uploads/") {
            return Err(ApiError::bad_request("backup contains unexpected entry"));
        }
        safe_zip_path(&name)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(ApiError::bad_request("backup symlinks are forbidden"));
        }
        total = total.saturating_add(entry.size());
        if total > max_unpacked {
            return Err(ApiError::bad_request("backup unpacked size exceeds limit"));
        }
        if entry.compressed_size() > 0 && entry.size() / entry.compressed_size().max(1) > 1000 {
            return Err(ApiError::bad_request("suspicious backup compression ratio"));
        }
        let expected = manifest
            .file_hashes
            .get(&name)
            .ok_or_else(|| ApiError::bad_request("backup entry missing from signed manifest"))?;
        if hash_reader(&mut entry)? != *expected {
            return Err(ApiError::bad_request("backup file checksum mismatch"));
        }
        seen.insert(name);
    }
    if seen.len() != manifest.file_hashes.len()
        || manifest.file_hashes.keys().any(|k| !seen.contains(k))
    {
        return Err(ApiError::bad_request("signed backup file set mismatch"));
    }
    Ok((manifest, has_db, has_dump))
}

fn extract_named_file(archive_path: &Path, name: &str, dest: &Path) -> ApiResult<()> {
    let file = File::open(archive_path).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| ApiError::bad_request(format!("invalid zip: {e}")))?;
    let mut entry = zip
        .by_name(name)
        .map_err(|_| ApiError::bad_request(format!("备份包缺少 {name}")))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    let mut out = File::create(dest).map_err(|e| ApiError::internal(e.to_string()))?;
    io::copy(&mut entry, &mut out).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

pub fn prune_backups(cfg: &Config) -> ApiResult<()> {
    let keep = cfg.backup_keep.max(1);
    let mut items = list_backup_files(cfg)?;
    items.sort_by(|a, b| b.name.cmp(&a.name));
    for old in items.into_iter().skip(keep) {
        let path = cfg.backup_dir.join(&old.name);
        let _ = fs::remove_file(path);
        tracing::info!("pruned backup {}", old.name);
    }
    Ok(())
}

fn list_backup_files(cfg: &Config) -> ApiResult<Vec<BackupItem>> {
    fs::create_dir_all(&cfg.backup_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut items = Vec::new();
    let rd = fs::read_dir(&cfg.backup_dir).map_err(|e| ApiError::internal(e.to_string()))?;
    for entry in rd {
        let entry = entry.map_err(|e| ApiError::internal(e.to_string()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_valid_backup_name(&name) {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if !meta.is_file() {
            continue;
        }
        let engine = name
            .strip_prefix("saltedblog_")
            .and_then(|rest| rest.split('_').next())
            .map(|s| s.to_string());
        let created_at = meta.modified().ok().map_or_else(
            || {
                let parts: Vec<&str> = name.trim_end_matches(".zip").split('_').collect();
                if parts.len() == 4 && parts[2].len() == 8 && parts[3].len() == 6 {
                    format!(
                        "{}-{}-{}T{}:{}:{}Z",
                        &parts[2][0..4],
                        &parts[2][4..6],
                        &parts[2][6..8],
                        &parts[3][0..2],
                        &parts[3][2..4],
                        &parts[3][4..6],
                    )
                } else {
                    Utc::now().to_rfc3339()
                }
            },
            |t| chrono::DateTime::<Utc>::from(t).to_rfc3339(),
        );
        items.push(BackupItem {
            name,
            size_bytes: meta.len(),
            created_at,
            engine,
        });
    }
    items.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(items)
}

pub fn list_backups(cfg: &Config) -> ApiResult<Vec<BackupItem>> {
    list_backup_files(cfg)
}

pub fn delete_backup(cfg: &Config, name: &str) -> ApiResult<()> {
    let path = backup_path(cfg, name)?;
    if !path.exists() {
        return Err(ApiError::not_found());
    }
    fs::remove_file(&path).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(())
}

/// 校验已落盘的 zip 是否为合法备份包；可选按期望引擎重命名
pub fn validate_uploaded_backup(cfg: &Config, path: &Path) -> ApiResult<String> {
    let (manifest, has_db, has_dump) = read_manifest(cfg, path)?;
    if !has_db && !has_dump {
        let _ = fs::remove_file(path);
        return Err(ApiError::bad_request("备份包中缺少 blog.db 或 blog.dump"));
    }
    let engine = if has_dump && !has_db {
        "postgres"
    } else if has_db && !has_dump {
        "sqlite"
    } else if manifest.engines.len() == 1 {
        manifest.engines[0].as_str()
    } else if has_dump {
        "postgres"
    } else {
        "sqlite"
    };

    let stamp = Utc::now().format("%Y%m%d_%H%M%S");
    let final_name = format!("saltedblog_{engine}_{stamp}.zip");
    let final_path = cfg.backup_dir.join(&final_name);
    fs::rename(path, &final_path).map_err(|e| ApiError::internal(format!("rename upload: {e}")))?;
    prune_backups(cfg)?;
    Ok(final_name)
}

pub async fn create_backup(state: &AppState) -> ApiResult<String> {
    let cfg = &state.cfg;
    let engine = detect_engine(&cfg.database_url)?;
    ensure_disk_space(cfg, engine)?;
    fs::create_dir_all(&cfg.backup_dir).map_err(|e| ApiError::internal(e.to_string()))?;

    let stamp = Utc::now().format("%Y%m%d_%H%M%S");
    let name = format!("saltedblog_{}_{stamp}.zip", engine.as_str());
    let final_path = cfg.backup_dir.join(&name);
    let tmp_path = cfg.backup_dir.join(format!(".{name}.partial"));
    let work_dir = cfg.backup_dir.join(format!(".work_{stamp}"));
    let _ = fs::remove_dir_all(&work_dir);
    fs::create_dir_all(&work_dir).map_err(|e| ApiError::internal(e.to_string()))?;

    let cleanup = |work: &Path, tmp: &Path| {
        let _ = fs::remove_dir_all(work);
        let _ = fs::remove_file(tmp);
    };

    let result = async {
        match engine {
            DbEngine::Sqlite => {
                let dest = work_dir.join("blog.db");
                dump_sqlite(&state.db(), &dest).await?;
            }
            DbEngine::Postgres => {
                let dest = work_dir.join("blog.dump");
                let url = cfg.database_maintenance_url.clone();
                let dest_c = dest.clone();
                tokio::task::spawn_blocking(move || dump_postgres(&url, &dest_c))
                    .await
                    .map_err(|e| ApiError::internal(format!("join pg_dump: {e}")))??;
            }
        }

        let file_hashes = collect_backup_hashes(&work_dir, &cfg.upload_dir, engine)?;
        let mut manifest = Manifest {
            format_version: FORMAT_VERSION,
            created_at: Utc::now().to_rfc3339(),
            engines: vec![engine.as_str().to_string()],
            app: "saltedblog".to_string(),
            file_hashes,
            signature: String::new(),
        };
        manifest.signature = sign_manifest(cfg, &manifest)?;
        let manifest_path = work_dir.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).map_err(|e| ApiError::internal(e.to_string()))?,
        )
        .map_err(|e| ApiError::internal(e.to_string()))?;

        let upload_dir = cfg.upload_dir.clone();
        let work = work_dir.clone();
        let tmp = tmp_path.clone();
        let eng = engine;
        tokio::task::spawn_blocking(move || -> ApiResult<()> {
            let file = File::create(&tmp).map_err(|e| ApiError::internal(e.to_string()))?;
            let mut zip = ZipWriter::new(file);
            add_file_to_zip(
                &mut zip,
                "manifest.json",
                &work.join("manifest.json"),
                false,
            )?;
            match eng {
                DbEngine::Sqlite => {
                    add_file_to_zip(&mut zip, "blog.db", &work.join("blog.db"), true)?;
                }
                DbEngine::Postgres => {
                    add_file_to_zip(&mut zip, "blog.dump", &work.join("blog.dump"), true)?;
                }
            }
            add_dir_to_zip(&mut zip, "uploads", &upload_dir)?;
            zip.finish()
                .map_err(|e| ApiError::internal(format!("zip finish: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| ApiError::internal(format!("join zip: {e}")))??;

        fs::rename(&tmp_path, &final_path).map_err(|e| ApiError::internal(e.to_string()))?;
        Ok::<_, ApiError>(())
    }
    .await;

    cleanup(&work_dir, &tmp_path);
    result?;
    prune_backups(cfg)?;
    tracing::info!("created backup {name}");
    Ok(name)
}

pub async fn restore_backup(state: &AppState, name: &str) -> ApiResult<()> {
    let cfg = &state.cfg;
    let engine = detect_engine(&cfg.database_url)?;
    let archive_path = backup_path(cfg, name)?;
    if !archive_path.exists() {
        return Err(ApiError::not_found());
    }

    let archive_path_owned = archive_path.clone();
    let verify_cfg = cfg.clone();
    let (manifest, has_db, has_dump) =
        tokio::task::spawn_blocking(move || read_manifest(&verify_cfg, &archive_path_owned))
            .await
            .map_err(|e| ApiError::internal(format!("join: {e}")))??;

    match engine {
        DbEngine::Sqlite if !has_db => {
            return Err(ApiError::bad_request(
                "当前为 SQLite，备份包中缺少 blog.db，无法恢复（不做跨引擎转换）",
            ));
        }
        DbEngine::Postgres if !has_dump => {
            return Err(ApiError::bad_request(
                "当前为 PostgreSQL，备份包中缺少 blog.dump，无法恢复（不做跨引擎转换）",
            ));
        }
        _ => {}
    }
    let _ = manifest;

    // 恢复前先打安全备份
    tracing::info!("creating safety backup before restore of {name}");
    create_backup(state).await?;

    let stamp = Utc::now().format("%Y%m%d_%H%M%S");
    let work_dir = cfg.backup_dir.join(format!(".restore_{stamp}"));
    let _ = fs::remove_dir_all(&work_dir);
    fs::create_dir_all(&work_dir).map_err(|e| ApiError::internal(e.to_string()))?;

    let cleanup_work = |work: &Path| {
        let _ = fs::remove_dir_all(work);
    };

    let result = async {
        let staged_uploads = work_dir.join("uploads_staged");
        let arch = archive_path.clone();
        let staged = staged_uploads.clone();
        tokio::task::spawn_blocking(move || extract_uploads_from_zip(&arch, &staged))
            .await
            .map_err(|e| ApiError::internal(format!("join: {e}")))??;

        match engine {
            DbEngine::Sqlite => {
                let extracted = work_dir.join("blog.db");
                let arch = archive_path.clone();
                let dest = extracted.clone();
                tokio::task::spawn_blocking(move || extract_named_file(&arch, "blog.db", &dest))
                    .await
                    .map_err(|e| ApiError::internal(format!("join: {e}")))??;

                let sqlite_path = sqlite_path_from_url(&cfg.database_url)?;
                if let Some(parent) = sqlite_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
                }
                let sidecars = [
                    PathBuf::from(format!("{}-wal", sqlite_path.display())),
                    PathBuf::from(format!("{}-shm", sqlite_path.display())),
                    PathBuf::from(format!("{}.bak", sqlite_path.display())),
                ];
                // 先放到旁路，再替换
                let staged = PathBuf::from(format!("{}.restoring", sqlite_path.display()));
                fs::copy(&extracted, &staged).map_err(|e| ApiError::internal(e.to_string()))?;
                verify_sqlite_integrity(&staged).await?;

                // 换连接前尽量关掉旧池：用新文件建立连接
                let new_url = cfg.database_url.clone();
                // 替换文件：Windows 上若旧连接仍占用可能失败；先 connect 新文件路径的旁路再 rename
                // 策略：rename staged over target，然后 reconnect
                if sqlite_path.exists() {
                    let bak = PathBuf::from(format!("{}.pre_restore", sqlite_path.display()));
                    let _ = fs::remove_file(&bak);
                    fs::rename(&sqlite_path, &bak).map_err(|e| {
                        ApiError::internal(format!("无法替换 SQLite 文件（可能仍被占用）: {e}"))
                    })?;
                    if let Err(e) = fs::rename(&staged, &sqlite_path) {
                        let _ = fs::rename(&bak, &sqlite_path);
                        return Err(ApiError::internal(format!("restore rename failed: {e}")));
                    }
                    let _ = fs::remove_file(&bak);
                } else {
                    fs::rename(&staged, &sqlite_path)
                        .map_err(|e| ApiError::internal(e.to_string()))?;
                }
                for s in &sidecars {
                    let _ = fs::remove_file(s);
                }

                let new_db = Database::connect(&new_url)
                    .await
                    .map_err(|e| ApiError::internal(format!("reconnect after restore: {e}")))?;
                state.replace_db(new_db);
            }
            DbEngine::Postgres => {
                let extracted = work_dir.join("blog.dump");
                let arch = archive_path.clone();
                let dest = extracted.clone();
                tokio::task::spawn_blocking(move || extract_named_file(&arch, "blog.dump", &dest))
                    .await
                    .map_err(|e| ApiError::internal(format!("join: {e}")))??;

                let url = cfg.database_maintenance_url.clone();
                let sql = extracted.clone();
                tokio::task::spawn_blocking(move || restore_postgres(&url, &sql))
                    .await
                    .map_err(|e| ApiError::internal(format!("join: {e}")))??;
            }
        }

        let upload_dir = cfg.upload_dir.clone();
        let swap_stamp = stamp.to_string();
        tokio::task::spawn_blocking(move || {
            swap_upload_dirs(&staged_uploads, &upload_dir, &swap_stamp)
        })
        .await
        .map_err(|e| ApiError::internal(format!("join: {e}")))??;

        Ok::<_, ApiError>(())
    }
    .await;

    cleanup_work(&work_dir);
    result?;
    prune_backups(cfg)?;
    tracing::info!("restored backup {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            app_env: "test".into(),
            database_url: "sqlite::memory:".into(),
            database_maintenance_url: "sqlite::memory:".into(),
            bind_addr: "127.0.0.1:0".into(),
            upload_dir: PathBuf::from("uploads"),
            upload_max_mb: 20,
            session_idle_minutes: 30,
            session_absolute_hours: 12,
            step_up_minutes: 5,
            cookie_secure: false,
            admin_origin: "http://localhost".into(),
            trusted_proxy_cidrs: Vec::new(),
            mfa_required: true,
            mfa_encryption_key: "test-mfa-key".into(),
            backup_signing_key: "test-backup-signing-key".into(),
            stats_tz_offset_hours: 8,
            admin_username: "admin".into(),
            admin_password: "password".into(),
            backup_dir: PathBuf::from("backups"),
            backup_keep: 7,
            backup_upload_max_mb: 1024,
            news_llm_api_key: String::new(),
        }
    }

    #[test]
    fn manifest_signature_detects_tampering() {
        let cfg = test_config();
        let mut manifest = Manifest {
            format_version: FORMAT_VERSION,
            created_at: "2026-07-17T00:00:00Z".into(),
            engines: vec!["sqlite".into()],
            app: "saltedblog".into(),
            file_hashes: BTreeMap::from([("blog.db".into(), "abc".into())]),
            signature: String::new(),
        };
        manifest.signature = sign_manifest(&cfg, &manifest).unwrap();
        assert!(verify_manifest(&cfg, &manifest).is_ok());

        manifest
            .file_hashes
            .insert("blog.db".into(), "changed".into());
        assert!(verify_manifest(&cfg, &manifest).is_err());
    }

    #[test]
    fn zip_paths_reject_traversal_and_absolute_paths() {
        assert!(safe_zip_path("uploads/image.png").is_ok());
        assert!(safe_zip_path("../secret").is_err());
        assert!(safe_zip_path("/etc/passwd").is_err());
        assert!(safe_zip_path("C:\\Windows\\system.ini").is_err());
    }
}
