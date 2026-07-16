//! 备份 / 恢复：统一 zip 格式（manifest + 可选 blog.db/blog.sql + uploads/）

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::Utc;
use fs2::available_space;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const FORMAT_VERSION: u32 = 1;

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
    let need = db_bytes.saturating_add(upload_bytes).saturating_add(32 * 1024 * 1024);
    let avail = available_space(&cfg.backup_dir).map_err(|e| {
        ApiError::internal(format!("failed to check disk space: {e}"))
    })?;
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

fn add_file_to_zip(zip: &mut ZipWriter<File>, name: &str, path: &Path, stored: bool) -> ApiResult<()> {
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
    let output = Command::new("pg_dump")
        .arg(database_url)
        .arg("--no-owner")
        .arg("--no-acl")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| {
            ApiError::internal(format!(
                "failed to run pg_dump (is postgresql-client installed?): {e}"
            ))
        })?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(ApiError::internal(format!("pg_dump failed: {err}")));
    }
    fs::write(dest, &output.stdout)
        .map_err(|e| ApiError::internal(format!("write blog.sql: {e}")))?;
    Ok(())
}

fn restore_postgres(database_url: &str, sql_path: &Path) -> ApiResult<()> {
    // 清空 public schema 后导入
    let drop = Command::new("psql")
        .arg(database_url)
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-c")
        .arg("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .output()
        .map_err(|e| ApiError::internal(format!("failed to run psql: {e}")))?;
    if !drop.status.success() {
        let err = String::from_utf8_lossy(&drop.stderr);
        return Err(ApiError::internal(format!("psql drop schema failed: {err}")));
    }
    let import = Command::new("psql")
        .arg(database_url)
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-f")
        .arg(sql_path)
        .output()
        .map_err(|e| ApiError::internal(format!("failed to run psql import: {e}")))?;
    if !import.status.success() {
        let err = String::from_utf8_lossy(&import.stderr);
        return Err(ApiError::internal(format!("psql import failed: {err}")));
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

fn extract_uploads_from_zip(archive_path: &Path, upload_dir: &Path) -> ApiResult<()> {
    let file = File::open(archive_path).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut zip = ZipArchive::new(file).map_err(|e| ApiError::bad_request(format!("invalid zip: {e}")))?;

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

fn read_manifest(archive_path: &Path) -> ApiResult<(Manifest, bool, bool)> {
    let file = File::open(archive_path).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut zip = ZipArchive::new(file).map_err(|e| ApiError::bad_request(format!("invalid zip: {e}")))?;

    let has_db = zip.by_name("blog.db").is_ok();
    let has_sql = zip.by_name("blog.sql").is_ok();

    let mut entry = zip
        .by_name("manifest.json")
        .map_err(|_| ApiError::bad_request("备份包缺少 manifest.json"))?;
    let mut buf = String::new();
    entry
        .read_to_string(&mut buf)
        .map_err(|e| ApiError::bad_request(format!("read manifest: {e}")))?;
    let manifest: Manifest = serde_json::from_str(&buf)
        .map_err(|e| ApiError::bad_request(format!("invalid manifest.json: {e}")))?;
    if manifest.format_version != FORMAT_VERSION {
        return Err(ApiError::bad_request(format!(
            "unsupported backup format_version: {}",
            manifest.format_version
        )));
    }
    Ok((manifest, has_db, has_sql))
}

fn extract_named_file(archive_path: &Path, name: &str, dest: &Path) -> ApiResult<()> {
    let file = File::open(archive_path).map_err(|e| ApiError::internal(e.to_string()))?;
    let mut zip = ZipArchive::new(file).map_err(|e| ApiError::bad_request(format!("invalid zip: {e}")))?;
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
        let meta = entry.metadata().map_err(|e| ApiError::internal(e.to_string()))?;
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
    let (manifest, has_db, has_sql) = read_manifest(path)?;
    if !has_db && !has_sql {
        let _ = fs::remove_file(path);
        return Err(ApiError::bad_request("备份包中缺少 blog.db 与 blog.sql"));
    }
    let engine = if has_sql && !has_db {
        "postgres"
    } else if has_db && !has_sql {
        "sqlite"
    } else if manifest.engines.len() == 1 {
        manifest.engines[0].as_str()
    } else if has_sql {
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
                let dest = work_dir.join("blog.sql");
                let url = cfg.database_url.clone();
                let dest_c = dest.clone();
                tokio::task::spawn_blocking(move || dump_postgres(&url, &dest_c))
                    .await
                    .map_err(|e| ApiError::internal(format!("join pg_dump: {e}")))??;
            }
        }

        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            created_at: Utc::now().to_rfc3339(),
            engines: vec![engine.as_str().to_string()],
            app: "saltedblog".to_string(),
        };
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
            add_file_to_zip(&mut zip, "manifest.json", &work.join("manifest.json"), false)?;
            match eng {
                DbEngine::Sqlite => {
                    add_file_to_zip(&mut zip, "blog.db", &work.join("blog.db"), true)?;
                }
                DbEngine::Postgres => {
                    add_file_to_zip(&mut zip, "blog.sql", &work.join("blog.sql"), false)?;
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
    let (manifest, has_db, has_sql) = tokio::task::spawn_blocking(move || read_manifest(&archive_path_owned))
        .await
        .map_err(|e| ApiError::internal(format!("join: {e}")))??;

    match engine {
        DbEngine::Sqlite if !has_db => {
            return Err(ApiError::bad_request(
                "当前为 SQLite，备份包中缺少 blog.db，无法恢复（不做跨引擎转换）",
            ));
        }
        DbEngine::Postgres if !has_sql => {
            return Err(ApiError::bad_request(
                "当前为 PostgreSQL，备份包中缺少 blog.sql，无法恢复（不做跨引擎转换）",
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

                // 换连接前尽量关掉旧池：用新文件建立连接
                let new_url = cfg.database_url.clone();
                // 替换文件：Windows 上若旧连接仍占用可能失败；先 connect 新文件路径的旁路再 rename
                // 策略：rename staged over target，然后 reconnect
                if sqlite_path.exists() {
                    let bak = PathBuf::from(format!("{}.pre_restore", sqlite_path.display()));
                    let _ = fs::remove_file(&bak);
                    fs::rename(&sqlite_path, &bak).map_err(|e| {
                        ApiError::internal(format!(
                            "无法替换 SQLite 文件（可能仍被占用）: {e}"
                        ))
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
                let extracted = work_dir.join("blog.sql");
                let arch = archive_path.clone();
                let dest = extracted.clone();
                tokio::task::spawn_blocking(move || extract_named_file(&arch, "blog.sql", &dest))
                    .await
                    .map_err(|e| ApiError::internal(format!("join: {e}")))??;

                let url = cfg.database_url.clone();
                let sql = extracted.clone();
                tokio::task::spawn_blocking(move || restore_postgres(&url, &sql))
                    .await
                    .map_err(|e| ApiError::internal(format!("join: {e}")))??;
            }
        }

        let arch = archive_path.clone();
        let upload_dir = cfg.upload_dir.clone();
        tokio::task::spawn_blocking(move || extract_uploads_from_zip(&arch, &upload_dir))
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
