use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// 单个后端的健康跟踪
pub struct BackendHealth {
    time_base: Instant,
    failure_count: AtomicUsize, // 当前连续失败次数
    cooldown_until: AtomicU64,  // 冷却结束时间戳（Unix 秒，0 表示无冷却）
}

impl BackendHealth {
    pub fn new() -> Self {
        Self {
            time_base: Instant::now(),
            failure_count: AtomicUsize::new(0),
            cooldown_until: AtomicU64::new(0),
        }
    }

    /// 记录成功的请求
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        self.cooldown_until.store(0, Ordering::Relaxed)
    }

    /// 记录失败的请求，如果后端应进入冷却则返回 true
    pub fn record_failure(&self, max_retries: usize) -> bool {
        let new_count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        new_count >= max_retries && self.cooldown_until.load(Ordering::Relaxed) == 0
    }

    /// 设置此后端的冷却
    pub fn set_cooldown(&self, duration: Duration) {
        self.cooldown_until.store(
            (self.time() + duration.as_secs_f64()).to_bits(),
            Ordering::Relaxed,
        )
    }

    /// 检查后端是否健康（不在冷却中或冷却已过期）
    pub fn is_healthy(&self) -> bool {
        let cooldown_until = self.cooldown_until.load(Ordering::Relaxed);
        cooldown_until == 0 || self.time() >= f64::from_bits(cooldown_until)
    }

    fn time(&self) -> f64 {
        self.time_base.elapsed().as_secs_f64()
    }
}

impl Default for BackendHealth {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_health_is_healthy() {
        let health = BackendHealth::new();
        assert!(health.is_healthy());
    }

    #[test]
    fn test_record_success_resets_failure_count() {
        let health = BackendHealth::new();

        // 先记录一些失败
        health.record_failure(3);
        health.record_failure(3);

        // 记录成功应该重置失败计数
        health.record_success();

        // 再次失败应该从头开始计数
        assert!(!health.record_failure(3)); // 第 1 次失败
        assert!(!health.record_failure(3)); // 第 2 次失败
    }

    #[test]
    fn test_record_failure_under_threshold() {
        let health = BackendHealth::new();

        // 未达到阈值时返回 false
        assert!(!health.record_failure(3)); // 第 1 次
        assert!(!health.record_failure(3)); // 第 2 次
    }

    #[test]
    fn test_record_failure_at_threshold() {
        let health = BackendHealth::new();

        // 达到阈值时返回 true
        assert!(!health.record_failure(3)); // 第 1 次
        assert!(!health.record_failure(3)); // 第 2 次
        assert!(health.record_failure(3)); // 第 3 次，应返回 true
    }

    #[test]
    fn test_set_cooldown_makes_unhealthy() {
        let health = BackendHealth::new();

        // 记录足够多的失败以触发冷却
        for _ in 0..3 {
            health.record_failure(3);
        }

        // 设置冷却
        health.set_cooldown(Duration::from_secs(10));

        // 冷却期间应不健康
        assert!(!health.is_healthy());
    }

    #[test]
    fn test_cooldown_expires() {
        let health = BackendHealth::new();

        // 触发冷却（需要达到阈值）
        health.record_failure(3);
        health.record_failure(3);
        health.set_cooldown(Duration::from_secs(1)); // 设置 1 秒冷却

        // 冷却期间不健康
        assert!(!health.is_healthy());

        // 等待冷却过期
        std::thread::sleep(Duration::from_millis(1100));

        // 冷却过期后应健康
        assert!(health.is_healthy());
    }

    #[test]
    fn test_multiple_success_resets_cooldown() {
        let health = BackendHealth::new();

        // 触发冷却
        for _ in 0..3 {
            health.record_failure(3);
        }
        health.set_cooldown(Duration::from_secs(10));

        // 冷却期间不健康
        assert!(!health.is_healthy());

        // 记录成功应该重置冷却
        health.record_success();

        // 应立即健康
        assert!(health.is_healthy());
    }

    #[test]
    fn test_default() {
        let health = BackendHealth::default();
        assert!(health.is_healthy());
    }
}
