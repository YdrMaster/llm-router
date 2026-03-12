use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// 单个后端的健康跟踪
pub struct BackendHealth {
    pub is_healthy: AtomicBool,     // 是否在健康状态
    pub failure_count: AtomicUsize, // 当前连续失败次数
    pub cooldown_until: Mutex<Instant>,
}

impl BackendHealth {
    pub fn new() -> Self {
        Self {
            is_healthy: AtomicBool::new(true),
            failure_count: AtomicUsize::new(0),
            cooldown_until: Mutex::new(Instant::now()),
        }
    }

    /// 记录成功的请求
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        self.is_healthy.store(true, Ordering::Relaxed);
    }

    /// 记录失败的请求，如果后端应进入冷却则返回 true
    pub fn record_failure(&self, max_retries: usize) -> bool {
        // 检查是否应进入冷却（超过重试限制）
        self.failure_count.fetch_add(1, Ordering::Relaxed) + 1 >= max_retries
            && self.is_healthy.load(Ordering::Relaxed)
    }

    /// 设置此后端的冷却
    pub fn set_cooldown(&self, duration: Duration) {
        self.is_healthy.store(false, Ordering::Relaxed);
        *self.cooldown_until.lock().unwrap() = Instant::now() + duration
    }

    /// 检查后端是否健康（不在冷却中或冷却已过期）
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Relaxed)
            || Instant::now() >= *self.cooldown_until.lock().unwrap()
    }

    /// 获取剩余冷却时间（如果有）
    pub fn remaining_cooldown(&self) -> Option<Duration> {
        self.cooldown_until
            .lock()
            .unwrap()
            .checked_duration_since(Instant::now())
    }
}

impl Default for BackendHealth {
    fn default() -> Self {
        Self::new()
    }
}
