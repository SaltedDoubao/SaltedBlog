use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub upload_dir: PathBuf,
    pub upload_max_mb: usize,
    pub session_ttl_days: i64,
    pub cookie_secure: bool,
    pub stats_tz_offset_hours: i32,
    pub admin_username: String,
    pub admin_password: String,
    pub backup_dir: PathBuf,
    pub backup_keep: usize,
    pub backup_upload_max_mb: usize,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: env_or("DATABASE_URL", "sqlite://data/blog.db?mode=rwc"),
            bind_addr: env_or("BIND_ADDR", "0.0.0.0:8787"),
            upload_dir: PathBuf::from(env_or("UPLOAD_DIR", "data/uploads")),
            upload_max_mb: env_or("UPLOAD_MAX_MB", "20").parse().unwrap_or(20),
            session_ttl_days: env_or("SESSION_TTL_DAYS", "30").parse().unwrap_or(30),
            cookie_secure: env_or("COOKIE_SECURE", "false") == "true",
            stats_tz_offset_hours: env_or("STATS_TZ_OFFSET_HOURS", "8").parse().unwrap_or(8),
            admin_username: env_or("ADMIN_USERNAME", "admin"),
            admin_password: env_or("ADMIN_PASSWORD", ""),
            backup_dir: PathBuf::from(env_or("BACKUP_DIR", "backups")),
            backup_keep: env_or("BACKUP_KEEP", "7").parse().unwrap_or(7),
            backup_upload_max_mb: env_or("BACKUP_UPLOAD_MAX_MB", "1024").parse().unwrap_or(1024),
        }
    }
}
