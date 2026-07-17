use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jieba_rs::Jieba;
use sea_orm::DatabaseConnection;
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use uuid::Uuid;

use crate::config::Config;

#[derive(Default)]
struct AttemptBucket {
    failures: Vec<Instant>,
    blocked_until: Option<Instant>,
}

#[derive(Default)]
struct LimiterState {
    ips: HashMap<IpAddr, AttemptBucket>,
    accounts: HashMap<String, AttemptBucket>,
    global: AttemptBucket,
}

/// 登录限流：同时按来源 IP、账号和全局维度实施指数退避。
pub struct LoginLimiter {
    state: Mutex<LimiterState>,
}

const WINDOW_SECS: u64 = 900; // 15 分钟
const IP_THRESHOLD: usize = 5;
const ACCOUNT_THRESHOLD: usize = 5;
const GLOBAL_THRESHOLD: usize = 50;

fn prune(bucket: &mut AttemptBucket, now: Instant) {
    bucket
        .failures
        .retain(|at| now.duration_since(*at).as_secs() < WINDOW_SECS);
    if bucket.blocked_until.is_some_and(|until| until <= now) {
        bucket.blocked_until = None;
    }
}

fn record_bucket(bucket: &mut AttemptBucket, threshold: usize, now: Instant) {
    prune(bucket, now);
    bucket.failures.push(now);
    if bucket.failures.len() >= threshold {
        let exponent = (bucket.failures.len() - threshold).min(9) as u32;
        let delay = Duration::from_secs((1u64 << exponent).min(600));
        bucket.blocked_until = Some(now + delay);
    }
}

fn retry_for(bucket: &mut AttemptBucket, now: Instant) -> Option<Duration> {
    prune(bucket, now);
    bucket
        .blocked_until
        .and_then(|until| until.checked_duration_since(now))
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LimiterState::default()),
        }
    }

    pub fn retry_after(&self, ip: IpAddr, account: &str) -> Option<Duration> {
        let mut state = self.state.lock().expect("login limiter lock");
        let now = Instant::now();
        let account = account.trim().to_ascii_lowercase();
        let ip_delay = state.ips.get_mut(&ip).and_then(|v| retry_for(v, now));
        let account_delay = state
            .accounts
            .get_mut(&account)
            .and_then(|v| retry_for(v, now));
        let global_delay = retry_for(&mut state.global, now);
        [ip_delay, account_delay, global_delay]
            .into_iter()
            .flatten()
            .max()
    }

    pub fn record_failure(&self, ip: IpAddr, account: &str) {
        let mut state = self.state.lock().expect("login limiter lock");
        let now = Instant::now();
        let account = account.trim().to_ascii_lowercase();
        record_bucket(state.ips.entry(ip).or_default(), IP_THRESHOLD, now);
        if state.accounts.len() < 10_000 || state.accounts.contains_key(&account) {
            record_bucket(
                state.accounts.entry(account).or_default(),
                ACCOUNT_THRESHOLD,
                now,
            );
        }
        record_bucket(&mut state.global, GLOBAL_THRESHOLD, now);
    }

    pub fn clear(&self, ip: IpAddr, account: &str) {
        let mut state = self.state.lock().expect("login limiter lock");
        state.ips.remove(&ip);
        state.accounts.remove(&account.trim().to_ascii_lowercase());
    }
}

#[cfg(test)]
mod limiter_tests {
    use super::*;

    #[test]
    fn blocks_by_ip_and_account_after_threshold() {
        let limiter = LoginLimiter::new();
        let ip: IpAddr = "203.0.113.8".parse().unwrap();
        for _ in 0..IP_THRESHOLD {
            limiter.record_failure(ip, "Admin");
        }
        assert!(limiter.retry_after(ip, "admin").is_some());
        assert!(limiter
            .retry_after("203.0.113.9".parse().unwrap(), "ADMIN")
            .is_some());
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
    pub log_tx: mpsc::Sender<crate::logging::NewEvent>,
    log_rx: AsyncMutex<Option<mpsc::Receiver<crate::logging::NewEvent>>>,
}

impl AppStateInner {
    pub fn new(db: DatabaseConnection, cfg: Config, jieba: Jieba, limiter: LoginLimiter) -> Self {
        let (log_tx, log_rx) = mpsc::channel(2048);
        Self {
            db_slot: std::sync::RwLock::new(db),
            cfg,
            jieba,
            limiter,
            track_salt: Uuid::new_v4().to_string(),
            backup_lock: AsyncMutex::new(()),
            backup_jobs: Mutex::new(HashMap::new()),
            log_tx,
            log_rx: AsyncMutex::new(Some(log_rx)),
        }
    }

    /// 获取当前数据库连接（Clone 池句柄，开销很低）
    pub fn db(&self) -> DatabaseConnection {
        self.db_slot.read().expect("db lock poisoned").clone()
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

    pub async fn take_log_receiver(&self) -> Option<mpsc::Receiver<crate::logging::NewEvent>> {
        self.log_rx.lock().await.take()
    }
}

pub type AppState = std::sync::Arc<AppStateInner>;
