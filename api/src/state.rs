use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use jieba_rs::Jieba;
use sea_orm::DatabaseConnection;
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

pub struct AppStateInner {
    db_slot: std::sync::RwLock<DatabaseConnection>,
    pub cfg: Config,
    pub jieba: Jieba,
    pub limiter: LoginLimiter,
    /// 统计去重使用的服务端盐（进程启动时随机生成即可满足按日去重）
    pub track_salt: String,
}

impl AppStateInner {
    pub fn new(db: DatabaseConnection, cfg: Config, jieba: Jieba, limiter: LoginLimiter) -> Self {
        Self {
            db_slot: std::sync::RwLock::new(db),
            cfg,
            jieba,
            limiter,
            track_salt: Uuid::new_v4().to_string(),
        }
    }

    /// 获取当前数据库连接（Clone 池句柄，开销很低）
    pub fn db(&self) -> DatabaseConnection {
        self.db_slot
            .read()
            .expect("db lock poisoned")
            .clone()
    }
}

pub type AppState = std::sync::Arc<AppStateInner>;
