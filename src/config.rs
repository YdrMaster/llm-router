use log::{LevelFilter, warn};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::Path;
use std::time::Duration;
use toml::Value;

#[derive(Debug)]
pub struct Config {
    pub service: ServiceConfig,
    pub backend: HashMap<String, BackendConfig>,
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

/// 扁平化可能因键中包含点号而嵌套的 TOML 表，
/// 但保留看起来像后端详情结构的表。
fn flatten_table(table: &toml::Table, prefix: &str) -> HashMap<String, Value> {
    let mut result = HashMap::new();

    // 如果此表看起来像后端详情结构，不要扁平化
    if table.contains_key("base-url") {
        result.insert(prefix.to_string(), Value::Table(table.clone()));
        return result;
    }

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
}
