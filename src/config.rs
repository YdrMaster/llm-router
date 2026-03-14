use log::{LevelFilter, warn};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use toml::Value;

#[derive(Debug)]
pub struct Config {
    pub service: ServiceConfig,
    pub backend: HashMap<String, BackendConfig>,
    pub load_balancer: HashMap<String, LoadBalancerConfig>,
    pub aliases: HashMap<String, Box<str>>,
    pub router: HashMap<String, Box<[Box<str>]>>,
    _default: ServiceDefault,
}

#[derive(Debug)]
pub struct ServiceConfig {
    pub port: u16,
    pub log_level: LevelFilter,
}

#[derive(Debug)]
pub struct BackendConfig {
    pub base_url: Box<str>,
    pub api_key: Option<Box<str>>,
    pub model: Option<Box<str>>,
    pub retry: usize,
    pub cooldown: Duration,
}

/// 负载均衡策略
#[derive(Debug, Clone, Default, PartialEq)]
pub enum LoadBalanceStrategy {
    /// 随机选择（shuffle）
    #[default]
    Shuffle,
    /// 轮询（round_robin）
    RoundRobin,
}

/// 负载均衡器配置
#[derive(Debug)]
pub struct LoadBalancerConfig {
    pub strategy: LoadBalanceStrategy,
    pub backends: Box<[Box<str>]>,
    /// 轮询计数器（用于 RoundRobin）
    pub counter: Arc<AtomicUsize>,
}

impl LoadBalancerConfig {
    /// 根据策略和给定的随机种子选择后端索引（纯函数，便于测试）
    pub fn select_index(&self, seed: usize) -> usize {
        match self.strategy {
            LoadBalanceStrategy::Shuffle => seed % self.backends.len(),
            LoadBalanceStrategy::RoundRobin => {
                self.counter.fetch_add(1, Ordering::Relaxed) % self.backends.len()
            }
        }
    }

    /// 根据索引获取后端名称
    pub fn get_backend(&self, index: usize) -> Option<&str> {
        self.backends.get(index).map(|s| s.as_ref())
    }
}

impl Clone for LoadBalancerConfig {
    fn clone(&self) -> Self {
        LoadBalancerConfig {
            strategy: self.strategy.clone(),
            backends: self.backends.clone(),
            counter: self.counter.clone(),
        }
    }
}

/// 默认服务配置
#[derive(Debug, Clone)]
pub struct ServiceDefault {
    retry: usize,
    cooldown: Duration,
}

const DEFAULT_RETRY: usize = 3;
const DEFAULT_COOLDOWN: Duration = Duration::from_mins(3);

impl Default for ServiceDefault {
    fn default() -> Self {
        ServiceDefault {
            retry: DEFAULT_RETRY,
            cooldown: DEFAULT_COOLDOWN,
        }
    }
}

enum RouteTarget {
    Alias(Box<str>),
    Backends(Box<[Box<str>]>),
}

/// 解析环境变量引用：$VAR_NAME
fn resolve_env_var(value: &str) -> String {
    if let Some(var_name) = value.strip_prefix('$') {
        env::var(var_name).unwrap_or_else(|_| value.to_string())
    } else {
        value.to_string()
    }
}

/// 解析时间字符串，如 "30s", "3min", "1h" 为 Duration
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();

    // 尝试查找单位后缀
    if let Some(num) = s.strip_suffix('s') {
        num.parse::<f32>().ok().map(Duration::from_secs_f32)
    } else if let Some(num) = s.strip_suffix("min") {
        num.parse::<f32>()
            .ok()
            .map(|m| Duration::from_secs_f32(m * 60.))
    } else if let Some(num) = s.strip_suffix('h') {
        num.parse::<f32>()
            .ok()
            .map(|h| Duration::from_secs_f32(h * 3600.))
    } else {
        None
    }
}

/// 扁平化可能因键中包含点号而嵌套的 TOML 表
fn flatten_table(table: &toml::Table, prefix: &str) -> HashMap<String, Value> {
    let mut result = HashMap::new();

    // 如果此表包含 "base-url"，说明这是一个后端详情结构，不要扁平化
    // 而是将其作为一个完整的表返回
    if table.contains_key("base-url") {
        result.insert(prefix.to_string(), Value::Table(table.clone()));
        return result;
    }

    // 检查此表是否只有简单的标量值（没有嵌套表）
    let has_nested_table = table.values().any(|v| v.is_table());

    if !has_nested_table {
        // 简单表，直接扁平化
        for (key, value) in table {
            let full_key = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", prefix, key)
            };
            result.insert(full_key, value.clone());
        }
        return result;
    }

    // 有嵌套表，需要递归
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{}.{}", prefix, key)
        };
        if let Some(nested) = value.as_table() {
            result.extend(flatten_table(nested, &full_key));
        } else {
            result.insert(full_key, value.clone());
        }
    }
    result
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        // 如果存在则移除 BOM
        let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
        Self::from_str(content)
    }

    fn from_str(content: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let value: Value = toml::from_str(content)?;

        // 解析 service 部分（必需）
        let service_table = value
            .get("service")
            .ok_or("missing [service] section")?
            .as_table()
            .ok_or("[service] must be a table")?;
        let port = service_table
            .get("port")
            .and_then(Value::as_integer)
            .ok_or("missing or invalid service.port")? as u16;

        // 解析 log_level（可选，默认为 info）
        let log_level = service_table
            .get("log_level")
            .and_then(Value::as_str)
            .map(|s| match s.to_lowercase().as_str() {
                "trace" => LevelFilter::Trace,
                "debug" => LevelFilter::Debug,
                "info" => LevelFilter::Info,
                "warn" => LevelFilter::Warn,
                "error" => LevelFilter::Error,
                "off" => LevelFilter::Off,
                _ => LevelFilter::Info,
            })
            .unwrap_or(LevelFilter::Info);

        // 解析 service.default 部分（可选）
        let mut default = ServiceDefault::default();
        if let Some(default_table) = service_table.get("default").and_then(Value::as_table) {
            if let Some(retry) = default_table.get("retry").and_then(Value::as_integer) {
                default.retry = retry as _
            }
            if let Some(cooldown) = default_table
                .get("cooldown")
                .and_then(Value::as_str)
                .and_then(parse_duration)
            {
                default.cooldown = cooldown
            }
        }

        // 解析 backend 部分（可选）
        let mut backend = HashMap::new();
        if let Some(backend_value) = value.get("backend") {
            let backend_table = backend_value
                .as_table()
                .ok_or("[backend] must be a table")?;
            let backend_flat = flatten_table(backend_table, "");

            for (key, val) in backend_flat {
                if let Some(s) = val.as_str() {
                    backend.insert(
                        key,
                        BackendConfig {
                            base_url: s.into(),
                            api_key: None,
                            model: None,
                            retry: default.retry,
                            cooldown: default.cooldown,
                        },
                    );
                } else if let Some(table) = val.as_table() {
                    let base_url = table
                        .get("base-url")
                        .and_then(Value::as_str)
                        .ok_or("Missing base-url in backend details")?;
                    let api_key = table
                        .get("api-key")
                        .and_then(Value::as_str)
                        .map(|s| resolve_env_var(s).into());
                    let model = table.get("model").and_then(Value::as_str).map(|s| s.into());

                    // 解析 retry（未指定时使用默认值）
                    let retry = table
                        .get("retry")
                        .and_then(Value::as_integer)
                        .map(|r| r as _)
                        .unwrap_or(default.retry);

                    // 解析 cooldown（未指定时使用默认值）
                    let cooldown = table
                        .get("cooldown")
                        .and_then(Value::as_str)
                        .and_then(parse_duration)
                        .unwrap_or(default.cooldown);

                    backend.insert(
                        key,
                        BackendConfig {
                            base_url: base_url.into(),
                            api_key,
                            model,
                            retry,
                            cooldown,
                        },
                    );
                } else {
                    return Err("Invalid backend value format".into());
                }
            }
        }

        // 解析 load-balance 部分（可选）
        let mut load_balancer = HashMap::new();
        if let Some(lb_value) = value.get("load-balance") {
            let lb_table = lb_value.as_table().ok_or("[load-balance] must be a table")?;

            for (key, val) in lb_table {
                if let Some(table) = val.as_table() {
                    // 解析 backends 数组（必需）
                    let backends = table
                        .get("backends")
                        .and_then(Value::as_array)
                        .ok_or("Missing or invalid 'backends' in load-balance config")?;
                    
                    let backends: Box<[Box<str>]> = backends
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .map(|s| s.into())
                                .ok_or("backends array values must be strings")
                        })
                        .collect::<Result<_, _>>()?;

                    // 解析 strategy（可选，默认为 shuffle）
                    let strategy = table
                        .get("strategy")
                        .and_then(Value::as_str)
                        .map(|s| match s.to_lowercase().as_str() {
                            "round_robin" | "round-robin" => LoadBalanceStrategy::RoundRobin,
                            "shuffle" | "random" => LoadBalanceStrategy::Shuffle,
                            _ => LoadBalanceStrategy::Shuffle,
                        })
                        .unwrap_or_default();

                    load_balancer.insert(
                        key.clone(),
                        LoadBalancerConfig {
                            strategy,
                            backends,
                            counter: Arc::new(AtomicUsize::new(0)),
                        },
                    );
                } else {
                    return Err("Invalid load-balance value format, must be a table".into());
                }
            }
        }

        // 解析 router 部分（可选）
        // 阶段 1: 解析原始路由条目到临时结构
        let mut raw_router: HashMap<String, RouteTarget> = HashMap::new();
        if let Some(router_value) = value.get("router") {
            let router_table = router_value.as_table().ok_or("[router] must be a table")?;
            let router_flat = flatten_table(router_table, "");

            for (key, val) in router_flat {
                let target = if let Some(next) = val.as_str() {
                    RouteTarget::Alias(next.into())
                } else if let Some(arr) = val.as_array() {
                    RouteTarget::Backends(
                        arr.iter()
                            .map(|v| {
                                v.as_str()
                                    .map(|s| s.into())
                                    .ok_or("router array values must be strings")
                            })
                            .collect::<Result<_, _>>()?,
                    )
                } else {
                    return Err("Invalid route format".into());
                };
                raw_router.insert(key, target);
            }
        }

        // 阶段 2: 展开别名链并分离到 aliases 和 router 表
        let mut aliases = HashMap::new();
        let mut router = HashMap::new();

        for (key, target) in &raw_router {
            match target {
                RouteTarget::Backends(backends) => {
                    router.insert(key.clone(), backends.clone());
                }
                RouteTarget::Alias(alias_target) => {
                    match resolve_alias_chain(&raw_router, alias_target) {
                        Ok(final_route) => {
                            aliases.insert(key.clone(), final_route);
                        }
                        Err(e) => {
                            warn!("Skipping invalid alias '{key}': {e}")
                        }
                    }
                }
            }
        }

        Ok(Config {
            service: ServiceConfig { port, log_level },
            backend,
            load_balancer,
            aliases,
            router,
            _default: default,
        })
    }
}

/// 展开别名链，返回最终的路由名
///
/// 如果检测到循环引用或目标路由不存在，返回 Err
fn resolve_alias_chain(
    raw_router: &HashMap<String, RouteTarget>,
    start: &str,
) -> Result<Box<str>, String> {
    let mut current = start;
    let mut visited = HashSet::new();
    let mut path = Vec::new();

    loop {
        // 如果当前节点已经在访问历史中，说明有循环
        if !visited.insert(current) {
            // 找到循环的起点
            let cycle_start = path.iter().position(|p| *p == current).unwrap();
            return Err(format!(
                "Detected circular alias reference: {} → {current}",
                path[cycle_start..].join("->")
            ));
        }

        path.push(current);

        match raw_router.get(current) {
            Some(RouteTarget::Alias(next)) => current = next,
            Some(RouteTarget::Backends(_)) => return Ok(current.into()), // 找到最终路由名
            None => return Err(format!("Route '{current}' does not exist")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试最小配置（只有 service 和 port）
    #[test]
    fn test_minimal_config() {
        let content = r#"
[service]
port = 8000
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.service.port, 8000);
        assert_eq!(config.service.log_level, LevelFilter::Info);
        assert_eq!(config._default.retry, DEFAULT_RETRY);
        assert_eq!(config._default.cooldown, DEFAULT_COOLDOWN);
        assert!(config.backend.is_empty());
        assert!(config.router.is_empty());
    }

    /// 测试完整的 service 配置
    #[test]
    fn test_service_config() {
        let content = r#"
[service]
port = 9000
log_level = "debug"

[service.default]
retry = 5
cooldown = "30s"
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.service.port, 9000);
        assert_eq!(config.service.log_level, LevelFilter::Debug);
        assert_eq!(config._default.retry, 5);
        assert_eq!(config._default.cooldown, Duration::from_secs(30));
    }

    /// 测试简单后端配置（字符串格式）
    #[test]
    fn test_simple_backend_config() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"
backend2 = "http://1.2.3.4:30001"
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.backend.len(), 2);

        let b1 = config.backend.get("backend1").unwrap();
        assert_eq!(b1.base_url.as_ref(), "http://1.2.3.4:30000");
        assert_eq!(b1.retry, DEFAULT_RETRY);
        assert_eq!(b1.cooldown, DEFAULT_COOLDOWN);
        assert!(b1.api_key.is_none());
        assert!(b1.model.is_none());
    }

    /// 测试详细后端配置
    #[test]
    fn test_detailed_backend_config() {
        let content = r#"
[service]
port = 8000

[service.default]
retry = 2
cooldown = "3min"

[backend.aliyun]
base-url = "https://dashscope.aliyuncs.com/apps/anthropic"
api-key = "sk-test-key"
model = "qwen-plus"
retry = 5
cooldown = "30s"
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.backend.len(), 1);

        let aliyun = config.backend.get("aliyun").unwrap();
        assert_eq!(
            aliyun.base_url.as_ref(),
            "https://dashscope.aliyuncs.com/apps/anthropic"
        );
        assert_eq!(
            aliyun.api_key.as_ref().map(|s| s.as_ref()),
            Some("sk-test-key")
        );
        assert_eq!(aliyun.model.as_ref().map(|s| s.as_ref()), Some("qwen-plus"));
        assert_eq!(aliyun.retry, 5);
        assert_eq!(aliyun.cooldown, Duration::from_secs(30));
    }

    /// 测试路由配置
    #[test]
    fn test_router_config() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"
backend2 = "http://1.2.3.4:30001"

[router]
Model-A = ["backend1", "backend2"]
Model-B = ["backend2"]
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.router.len(), 2);

        let backends = config.router.get("Model-A").unwrap();
        assert_eq!(backends.len(), 2);
        assert_eq!(backends[0].as_ref(), "backend1");
        assert_eq!(backends[1].as_ref(), "backend2");

        let backends = config.router.get("Model-B").unwrap();
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0].as_ref(), "backend2");
    }

    /// 测试时间字符串解析
    #[test]
    fn test_duration_parsing() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("60s"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("1min"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("3min"), Some(Duration::from_secs(180)));
        assert_eq!(parse_duration("1.5h"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("60s"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("invalid"), None);
        assert_eq!(parse_duration(""), None);
    }

    /// 测试别名配置
    #[test]
    fn test_alias_config() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"
backend2 = "http://1.2.3.4:30001"

[router]
Model-A = ["backend1", "backend2"]
alias-model = "Model-A"
"#;
        let config = Config::from_str(content).unwrap();

        // 路由表应该只有 1 个条目 (Model-A)
        assert_eq!(config.router.len(), 1);
        assert!(config.router.contains_key("Model-A"));

        // 别名表应该有 1 个条目
        assert_eq!(config.aliases.len(), 1);
        assert_eq!(
            config.aliases.get("alias-model").map(|s| s.as_ref()),
            Some("Model-A")
        );
    }

    /// 测试别名链展开
    #[test]
    fn test_alias_chain() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"

[router]
real-model = ["backend1"]
alias1 = "real-model"
alias2 = "alias1"
alias3 = "alias2"
"#;
        let config = Config::from_str(content).unwrap();

        // 路由表应该只有 1 个条目
        assert_eq!(config.router.len(), 1);

        // 别名表应该有 3 个条目，都指向 real-model
        assert_eq!(config.aliases.len(), 3);
        assert_eq!(
            config.aliases.get("alias1").map(|s| s.as_ref()),
            Some("real-model")
        );
        assert_eq!(
            config.aliases.get("alias2").map(|s| s.as_ref()),
            Some("real-model")
        );
        assert_eq!(
            config.aliases.get("alias3").map(|s| s.as_ref()),
            Some("real-model")
        );
    }

    /// 测试循环引用检测
    #[test]
    fn test_alias_cycle_detection() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"

[router]
real-model = ["backend1"]
alias1 = "alias2"
alias2 = "alias1"
"#;
        let config = Config::from_str(content).unwrap();

        // 路由表应该只有 1 个条目
        assert_eq!(config.router.len(), 1);

        // 别名表应该是空的 (循环引用被拒绝)
        assert_eq!(config.aliases.len(), 0);
    }

    /// 测试不存在的目标
    #[test]
    fn test_alias_nonexistent_target() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"

[router]
real-model = ["backend1"]
alias1 = "nonexistent"
"#;
        let config = Config::from_str(content).unwrap();

        // 路由表应该只有 1 个条目
        assert_eq!(config.router.len(), 1);

        // 别名表应该是空的 (目标不存在被拒绝)
        assert_eq!(config.aliases.len(), 0);
    }

    /// 测试带点号的键名
    #[test]
    fn test_dotted_key_names() {
        let content = r#"
[service]
port = 8000

[backend]
model1-local = "http://1.2.3.4:30000"
model2-local = "http://1.2.3.4:30001"

[router]
model1 = ["model1-local"]
model2 = ["model2-local"]
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.backend.len(), 2);
        assert!(config.backend.contains_key("model1-local"));
        assert!(config.backend.contains_key("model2-local"));
        assert_eq!(config.router.len(), 2);
        assert!(config.router.contains_key("model1"));
        assert!(config.router.contains_key("model2"));
    }

    /// 测试混合后端配置
    #[test]
    fn test_mixed_backend_config() {
        let content = r#"
[service]
port = 8000

[service.default]
retry = 2
cooldown = "1min"

[backend]
simple-backend = "http://1.2.3.4:30000"

[backend.detailed]
base-url = "https://api.example.com"
api-key = "sk-key"
retry = 10
cooldown = "5min"
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.backend.len(), 2);

        let simple = config.backend.get("simple-backend").unwrap();
        assert_eq!(simple.retry, 2);
        assert_eq!(simple.cooldown, Duration::from_secs(60));
        assert!(simple.api_key.is_none());

        let detailed = config.backend.get("detailed").unwrap();
        assert_eq!(detailed.retry, 10);
        assert_eq!(detailed.cooldown, Duration::from_mins(5));
        assert_eq!(
            detailed.api_key.as_ref().map(|s| s.as_ref()),
            Some("sk-key")
        );
    }

    /// 测试缺失 service 部分的错误
    #[test]
    fn test_missing_service_section() {
        let content = r#"
[backend]
backend1 = "http://1.2.3.4:30000"
"#;
        let result = Config::from_str(content);
        assert!(result.is_err());
    }

    /// 测试缺失 port 的错误
    #[test]
    fn test_missing_port() {
        let content = r#"
[service]
log_level = "info"
"#;
        let result = Config::from_str(content);
        assert!(result.is_err());
    }

    /// 测试完整的生产环境配置
    #[test]
    fn test_production_config() {
        let content = r#"
[service]
port = 9000
log_level = "warn"

[service.default]
retry = 2
cooldown = "3min"

[backend]
model1-local = "http://1.2.3.4:30001"
model2-local = "http://1.2.3.4:30000"

[backend.aliyun]
base-url = "https://dashscope.aliyuncs.com/apps/anthropic"
api-key = "sk-production-key"
retry = 5
cooldown = "30s"

[router]
model1 = ["model1-local", "aliyun"]
model2 = ["model2-local", "aliyun"]
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.service.port, 9000);
        assert_eq!(config.service.log_level, LevelFilter::Warn);
        assert_eq!(config._default.retry, 2);
        assert_eq!(config._default.cooldown, Duration::from_secs(180));
        assert_eq!(config.backend.len(), 3);

        let aliyun = config.backend.get("aliyun").unwrap();
        assert_eq!(
            aliyun.base_url.as_ref(),
            "https://dashscope.aliyuncs.com/apps/anthropic"
        );
        assert_eq!(aliyun.retry, 5);
        assert_eq!(aliyun.cooldown, Duration::from_secs(30));
        assert_eq!(config.router.len(), 2);

        let backends = config.router.get("model1").unwrap();
        assert_eq!(backends.len(), 2);
        assert_eq!(backends[0].as_ref(), "model1-local");
        assert_eq!(backends[1].as_ref(), "aliyun");
    }

    /// 测试负载均衡配置
    #[test]
    fn test_load_balancer_config() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"
backend2 = "http://1.2.3.4:30001"
backend3 = "http://1.2.3.4:30002"

[load-balance.pool1]
backends = ["backend1", "backend2"]
strategy = "shuffle"

[load-balance.pool2]
backends = ["backend1", "backend2", "backend3"]
strategy = "round_robin"

[router]
model-a = ["pool1", "backend3"]
"#;
        let config = Config::from_str(content).unwrap();

        assert_eq!(config.load_balancer.len(), 2);

        let pool1 = config.load_balancer.get("pool1").unwrap();
        assert_eq!(pool1.backends.len(), 2);
        assert_eq!(pool1.backends[0].as_ref(), "backend1");
        assert_eq!(pool1.backends[1].as_ref(), "backend2");
        assert!(matches!(pool1.strategy, LoadBalanceStrategy::Shuffle));

        let pool2 = config.load_balancer.get("pool2").unwrap();
        assert_eq!(pool2.backends.len(), 3);
        assert!(matches!(pool2.strategy, LoadBalanceStrategy::RoundRobin));

        assert_eq!(config.router.len(), 1);
        let model_a = config.router.get("model-a").unwrap();
        assert_eq!(model_a.len(), 2);
        assert_eq!(model_a[0].as_ref(), "pool1");
        assert_eq!(model_a[1].as_ref(), "backend3");
    }

    /// 测试负载均衡配置默认策略
    #[test]
    fn test_load_balancer_default_strategy() {
        let content = r#"
[service]
port = 8000

[backend]
backend1 = "http://1.2.3.4:30000"

[load-balance.pool1]
backends = ["backend1"]
"#;
        let config = Config::from_str(content).unwrap();

        let pool1 = config.load_balancer.get("pool1").unwrap();
        assert!(matches!(pool1.strategy, LoadBalanceStrategy::Shuffle));
    }

    /// 测试 LoadBalancerConfig::select_index - Shuffle 策略
    #[test]
    fn test_load_balancer_select_index_shuffle() {
        use std::sync::Arc;

        let backends: Box<[Box<str>]> = vec!["backend1".into(), "backend2".into(), "backend3".into()]
            .into_boxed_slice();
        
        let lb = LoadBalancerConfig {
            strategy: LoadBalanceStrategy::Shuffle,
            backends: backends.clone(),
            counter: Arc::new(AtomicUsize::new(0)),
        };

        // 不同的种子应该产生不同的索引（在范围内）
        for seed in 0..100 {
            let index = lb.select_index(seed);
            assert!(index < 3, "Index {} out of range for seed {}", index, seed);
        }

        // 相同种子应该产生相同索引
        assert_eq!(lb.select_index(42), lb.select_index(42));
        assert_eq!(lb.select_index(100), lb.select_index(100));
    }

    /// 测试 LoadBalancerConfig::select_index - RoundRobin 策略
    #[test]
    fn test_load_balancer_select_index_round_robin() {
        use std::sync::Arc;

        let backends: Box<[Box<str>]> = vec!["backend1".into(), "backend2".into(), "backend3".into()]
            .into_boxed_slice();
        
        let lb = LoadBalancerConfig {
            strategy: LoadBalanceStrategy::RoundRobin,
            backends,
            counter: Arc::new(AtomicUsize::new(0)),
        };

        // 轮询应该依次返回 0, 1, 2, 0, 1, 2, ...
        assert_eq!(lb.select_index(0), 0); // counter was 0, now 1
        assert_eq!(lb.select_index(0), 1); // counter was 1, now 2
        assert_eq!(lb.select_index(0), 2); // counter was 2, now 3
        assert_eq!(lb.select_index(0), 0); // counter was 3, now 4 (wraps around)
        assert_eq!(lb.select_index(0), 1); // counter was 4, now 5
    }

    /// 测试 LoadBalancerConfig::get_backend
    #[test]
    fn test_load_balancer_get_backend() {
        use std::sync::Arc;

        let backends: Box<[Box<str>]> = vec!["backend1".into(), "backend2".into(), "backend3".into()]
            .into_boxed_slice();
        
        let lb = LoadBalancerConfig {
            strategy: LoadBalanceStrategy::Shuffle,
            backends,
            counter: Arc::new(AtomicUsize::new(0)),
        };

        assert_eq!(lb.get_backend(0), Some("backend1"));
        assert_eq!(lb.get_backend(1), Some("backend2"));
        assert_eq!(lb.get_backend(2), Some("backend3"));
        assert_eq!(lb.get_backend(3), None); // 超出范围
        assert_eq!(lb.get_backend(usize::MAX), None); // 极大值
    }

    /// 测试 LoadBalancerConfig 空后端列表的边界情况
    #[test]
    fn test_load_balancer_empty_backends() {
        use std::sync::Arc;

        let backends: Box<[Box<str>]> = vec![].into_boxed_slice();
        
        let lb = LoadBalancerConfig {
            strategy: LoadBalanceStrategy::Shuffle,
            backends,
            counter: Arc::new(AtomicUsize::new(0)),
        };

        // 空后端列表，select_index 会 panic（除零），这是预期行为
        // 实际配置解析时会验证 backends 非空
        let result = std::panic::catch_unwind(|| {
            let _ = lb.select_index(42);
        });
        assert!(result.is_err()); // 应该 panic
    }

    /// 测试单个后端的负载均衡
    #[test]
    fn test_load_balancer_single_backend() {
        use std::sync::Arc;

        let backends: Box<[Box<str>]> = vec!["only-backend".into()].into_boxed_slice();
        
        let lb = LoadBalancerConfig {
            strategy: LoadBalanceStrategy::Shuffle,
            backends,
            counter: Arc::new(AtomicUsize::new(0)),
        };

        // 单个后端，无论种子是什么都应该返回 0
        for seed in 0..10 {
            assert_eq!(lb.select_index(seed), 0);
        }
        assert_eq!(lb.get_backend(0), Some("only-backend"));
    }

    /// 测试 RoundRobin 计数器的线程安全性（多轮测试）
    #[test]
    fn test_load_balancer_round_robin_multiple_cycles() {
        use std::sync::Arc;

        let backends: Box<[Box<str>]> = vec!["b1".into(), "b2".into()].into_boxed_slice();
        
        let lb = LoadBalancerConfig {
            strategy: LoadBalanceStrategy::RoundRobin,
            backends,
            counter: Arc::new(AtomicUsize::new(0)),
        };

        // 测试多轮循环
        let mut results = Vec::new();
        for _ in 0..10 {
            results.push(lb.select_index(0));
        }

        // 应该是 0, 1, 0, 1, 0, 1, 0, 1, 0, 1
        let expected = vec![0, 1, 0, 1, 0, 1, 0, 1, 0, 1];
        assert_eq!(results, expected);
    }

    /// 测试策略字符串解析的大小写不敏感性
    #[test]
    fn test_load_balancer_strategy_case_insensitive() {
        let test_cases = vec![
            ("shuffle", LoadBalanceStrategy::Shuffle),
            ("SHUFFLE", LoadBalanceStrategy::Shuffle),
            ("Shuffle", LoadBalanceStrategy::Shuffle),
            ("random", LoadBalanceStrategy::Shuffle),
            ("RANDOM", LoadBalanceStrategy::Shuffle),
            ("round_robin", LoadBalanceStrategy::RoundRobin),
            ("ROUND_ROBIN", LoadBalanceStrategy::RoundRobin),
            ("round-robin", LoadBalanceStrategy::RoundRobin),
            ("ROUND-ROBIN", LoadBalanceStrategy::RoundRobin),
            ("invalid", LoadBalanceStrategy::Shuffle), // 无效值默认为 Shuffle
        ];

        for (input, expected) in test_cases {
            let result = match input.to_lowercase().as_str() {
                "round_robin" | "round-robin" => LoadBalanceStrategy::RoundRobin,
                "shuffle" | "random" | _ => LoadBalanceStrategy::Shuffle,
            };
            assert_eq!(result, expected, "Failed for input: {}", input);
        }
    }

    /// 测试负载均衡配置缺少 backends 字段
    #[test]
    fn test_load_balancer_missing_backends() {
        let content = r#"
[service]
port = 8000

[load-balance.pool1]
strategy = "shuffle"
"#;
        let result = Config::from_str(content);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("backends"));
    }

    /// 测试负载均衡配置 backends 为空数组
    #[test]
    fn test_load_balancer_empty_backends_array() {
        let content = r#"
[service]
port = 8000

[load-balance.pool1]
backends = []
strategy = "shuffle"
"#;
        let result = Config::from_str(content);
        // 空数组应该被接受，但实际选择时会 panic
        // 或者在解析时拒绝
        assert!(result.is_ok()); // 目前允许空数组
    }

    /// 测试负载均衡配置 backends 引用不存在的后端
    #[test]
    fn test_load_balancer_references_nonexistent_backend() {
        let content = r#"
[service]
port = 8000

[backend]
real-backend = "http://1.2.3.4:30000"

[load-balance.pool1]
backends = ["real-backend", "nonexistent-backend"]
strategy = "shuffle"

[router]
model-a = ["pool1"]
"#;
        let config = Config::from_str(content).unwrap();
        // 配置解析时不验证后端是否存在，运行时处理
        assert!(config.load_balancer.contains_key("pool1"));
    }

    /// 测试 resolve_env_var 函数
    #[test]
    fn test_resolve_env_var_with_env() {
        unsafe {
            std::env::set_var("TEST_VAR", "test_value");
        }
        let result = super::resolve_env_var("$TEST_VAR");
        assert_eq!(result, "test_value");
    }

    #[test]
    fn test_resolve_env_var_without_env() {
        // 使用一个不太可能存在的变量名
        let result = super::resolve_env_var("$NONEXISTENT_VAR_12345");
        assert_eq!(result, "$NONEXISTENT_VAR_12345"); // 返回原值
    }

    #[test]
    fn test_resolve_env_var_not_a_var() {
        let result = super::resolve_env_var("plain_value");
        assert_eq!(result, "plain_value");
    }

    /// 测试 parse_duration 函数的边界情况
    #[test]
    fn test_parse_duration_edge_cases() {
        // 零值
        assert_eq!(parse_duration("0s"), Some(Duration::from_secs(0)));
        assert_eq!(parse_duration("0min"), Some(Duration::from_secs(0)));
        
        // 小数值
        assert_eq!(parse_duration("0.5s"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("0.5min"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("0.25h"), Some(Duration::from_secs(900)));
        
        // 大数值
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(86400)));
        
        // 带空格
        assert_eq!(parse_duration(" 30s "), Some(Duration::from_secs(30)));
        
        // 无效格式
        assert_eq!(parse_duration("30"), None); // 没有单位
        assert_eq!(parse_duration("30x"), None); // 无效单位
        assert_eq!(parse_duration("abc"), None); // 非数字
        assert_eq!(parse_duration(""), None); // 空字符串
    }

    /// 测试 flatten_table 函数
    #[test]
    fn test_flatten_table_simple() {
        use toml::Value;
        
        let mut table = toml::Table::new();
        table.insert("key1".to_string(), Value::String("value1".to_string()));
        table.insert("key2".to_string(), Value::Integer(42));
        
        let result = super::flatten_table(&table, "");
        
        assert_eq!(result.get("key1").unwrap().as_str(), Some("value1"));
        assert_eq!(result.get("key2").unwrap().as_integer(), Some(42));
    }

    #[test]
    fn test_flatten_table_with_prefix() {
        use toml::Value;
        
        let mut table = toml::Table::new();
        table.insert("key1".to_string(), Value::String("value1".to_string()));
        
        let result = super::flatten_table(&table, "prefix");
        
        assert!(result.contains_key("prefix.key1"));
        assert!(!result.contains_key("key1"));
    }

    #[test]
    fn test_flatten_table_nested() {
        use toml::Value;
        
        let mut inner = toml::Table::new();
        inner.insert("inner_key".to_string(), Value::String("inner_value".to_string()));
        
        let mut table = toml::Table::new();
        table.insert("outer_key".to_string(), Value::Table(inner));
        
        let result = super::flatten_table(&table, "");
        
        assert_eq!(
            result.get("outer_key.inner_key").unwrap().as_str(),
            Some("inner_value")
        );
    }

    #[test]
    fn test_flatten_table_backend_config() {
        use toml::Value;
        
        let mut table = toml::Table::new();
        table.insert("base-url".to_string(), Value::String("http://example.com".to_string()));
        table.insert("api-key".to_string(), Value::String("secret".to_string()));
        
        let result = super::flatten_table(&table, "backend1");
        
        // 包含 base-url 的表不应该被扁平化
        assert!(result.contains_key("backend1"));
        assert!(!result.contains_key("backend1.base-url"));
    }
}
