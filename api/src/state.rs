use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use jieba_rs::Jieba;
use sea_orm::DatabaseConnection;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::config::Config;

/// 登录限流：每个 IP 在时间窗口内的失败尝试记录
pub struct LoginLimiter {
    attempts: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

const WINDOW_SECS: u64 = 900; // 15 分钟
const MAX_ATTEMPTS: usize = 5;

impl LoginLimiter {
    pub fn new() -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// 是否已被限流
    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let mut map = self.attempts.lock().unwrap();
        let now = Instant::now();
        map.retain(|_, list| {
            list.retain(|t| now.duration_since(*t).as_secs() < WINDOW_SECS);
            !list.is_empty()
        });
        map.get(&ip).is_some_and(|list| list.len() >= MAX_ATTEMPTS)
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut map = self.attempts.lock().unwrap();
        map.entry(ip).or_default().push(Instant::now());
    }

    pub fn clear(&self, ip: IpAddr) {
        let mut map = self.attempts.lock().unwrap();
        map.remove(&ip);
    }
}

#[derive(Clone, Debug)]
pub struct BackupJobStatus {
    pub status: String,
    pub error: Option<String>,
    pub backup_name: Option<String>,
}

pub struct AppStateInner {
    db_slot: std::sync::RwLock<DatabaseConnection>,
    pub cfg: Config,
    pub jieba: Jieba,
    pub limiter: LoginLimiter,
    /// 统计去重使用的服务端盐（进程启动时随机生成即可满足按日去重）
    pub track_salt: String,
    pub backup_lock: AsyncMutex<()>,
    pub backup_jobs: Mutex<HashMap<String, BackupJobStatus>>,
}

impl AppStateInner {
    pub fn new(db: DatabaseConnection, cfg: Config, jieba: Jieba, limiter: LoginLimiter) -> Self {
        Self {
            db_slot: std::sync::RwLock::new(db),
            cfg,
            jieba,
            limiter,
            track_salt: Uuid::new_v4().to_string(),
            backup_lock: AsyncMutex::new(()),
            backup_jobs: Mutex::new(HashMap::new()),
        }
    }

    /// 获取当前数据库连接（Clone 池句柄，开销很低）
    pub fn db(&self) -> DatabaseConnection {
        self.db_slot
            .read()
            .expect("db lock poisoned")
            .clone()
    }

    pub fn replace_db(&self, db: DatabaseConnection) {
        *self.db_slot.write().expect("db lock poisoned") = db;
    }

    pub fn set_job(&self, id: &str, status: BackupJobStatus) {
        self.backup_jobs
            .lock()
            .expect("backup_jobs lock")
            .insert(id.to_string(), status);
    }

    pub fn get_job(&self, id: &str) -> Option<BackupJobStatus> {
        self.backup_jobs
            .lock()
            .expect("backup_jobs lock")
            .get(id)
            .cloned()
    }
}

pub type AppState = std::sync::Arc<AppStateInner>;
