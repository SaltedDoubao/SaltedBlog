use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub app_env: String,
    pub database_url: String,
    pub database_maintenance_url: String,
    pub bind_addr: String,
    pub upload_dir: PathBuf,
    pub upload_max_mb: usize,
    pub session_idle_minutes: i64,
    pub session_absolute_hours: i64,
    pub step_up_minutes: i64,
    pub cookie_secure: bool,
    pub admin_origin: String,
    pub trusted_proxy_cidrs: Vec<ipnet::IpNet>,
    pub mfa_required: bool,
    pub mfa_encryption_key: String,
    pub backup_signing_key: String,
    pub stats_tz_offset_hours: i32,
    pub admin_username: String,
    pub admin_password: String,
    pub backup_dir: PathBuf,
    pub backup_keep: usize,
    pub backup_upload_max_mb: usize,
    /// LLM API Key（敏感信息仅走环境变量，不落库）
    pub news_llm_api_key: String,
}

fn env_or(key: &str, default: &str) -> String {
    if let Ok(file) = std::env::var(format!("{key}_FILE")) {
        if let Ok(value) = std::fs::read_to_string(file) {
            return value.trim().to_string();
        }
    }
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

impl Config {
    pub fn from_env() -> Self {
        let database_url = env_or("DATABASE_URL", "sqlite://data/blog.db?mode=rwc");
        Self {
            app_env: env_or("APP_ENV", "development"),
            database_maintenance_url: env_or("DATABASE_MAINTENANCE_URL", &database_url),
            database_url,
            bind_addr: env_or("BIND_ADDR", "0.0.0.0:8787"),
            upload_dir: PathBuf::from(env_or("UPLOAD_DIR", "data/uploads")),
            upload_max_mb: env_or("UPLOAD_MAX_MB", "20").parse().unwrap_or(20),
            session_idle_minutes: env_or("SESSION_IDLE_MINUTES", "30").parse().unwrap_or(30),
            session_absolute_hours: env_or("SESSION_ABSOLUTE_HOURS", "12").parse().unwrap_or(12),
            step_up_minutes: env_or("STEP_UP_MINUTES", "5").parse().unwrap_or(5),
            cookie_secure: env_or("COOKIE_SECURE", "false") == "true",
            admin_origin: env_or("ADMIN_ORIGIN", "http://localhost:4321")
                .trim_end_matches('/')
                .to_string(),
            trusted_proxy_cidrs: env_or("TRUSTED_PROXY_CIDRS", "")
                .split(',')
                .filter_map(|v| v.trim().parse().ok())
                .collect(),
            mfa_required: env_or("MFA_REQUIRED", "false") == "true",
            mfa_encryption_key: env_or("MFA_ENCRYPTION_KEY", ""),
            backup_signing_key: env_or("BACKUP_SIGNING_KEY", ""),
            stats_tz_offset_hours: env_or("STATS_TZ_OFFSET_HOURS", "8").parse().unwrap_or(8),
            admin_username: env_or("ADMIN_USERNAME", "admin"),
            admin_password: env_or("ADMIN_PASSWORD", ""),
            backup_dir: PathBuf::from(env_or("BACKUP_DIR", "backups")),
            backup_keep: env_or("BACKUP_KEEP", "7").parse().unwrap_or(7),
            backup_upload_max_mb: env_or("BACKUP_UPLOAD_MAX_MB", "1024")
                .parse()
                .unwrap_or(1024),
            news_llm_api_key: env_or("NEWS_LLM_API_KEY", ""),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.app_env == "production" {
            anyhow::ensure!(self.cookie_secure, "production requires COOKIE_SECURE=true");
            anyhow::ensure!(self.mfa_required, "production requires MFA_REQUIRED=true");
            anyhow::ensure!(
                !self.mfa_encryption_key.is_empty(),
                "production requires MFA_ENCRYPTION_KEY(_FILE)"
            );
            anyhow::ensure!(
                !self.backup_signing_key.is_empty(),
                "production requires BACKUP_SIGNING_KEY(_FILE)"
            );
            anyhow::ensure!(
                self.admin_origin.starts_with("https://"),
                "production ADMIN_ORIGIN must use https"
            );
            anyhow::ensure!(
                self.admin_password != "please-change-me",
                "refusing default ADMIN_PASSWORD"
            );
        }
        Ok(())
    }
}
