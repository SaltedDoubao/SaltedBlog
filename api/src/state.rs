use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use jieba_rs::Jieba;
use sea_orm::DatabaseConnection;

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
        if let Some(list) = map.get_mut(&ip) {
            list.retain(|t| now.duration_since(*t).as_secs() < WINDOW_SECS);
            list.len() >= MAX_ATTEMPTS
        } else {
            false
        }
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut map = self.attempts.lock().unwrap();
        map.entry(ip).or_default().push(Instant::now());
    }

    pub fn clear(&self, ip: IpAddr) {
        self.attempts.lock().unwrap().remove(&ip);
    }
}

pub struct AppStateInner {
    pub db: DatabaseConnection,
    pub cfg: Config,
    pub jieba: Jieba,
    pub limiter: LoginLimiter,
    /// 统计去重使用的服务端盐（进程启动时随机生成即可满足按日去重）
    pub track_salt: String,
}

pub type AppState = std::sync::Arc<AppStateInner>;
