mod config;
mod health;
mod logger;
mod protocol;
mod serve;

use std::env;

use config::Config;
use serve::serve;

fn main() {
    // 加载配置文件
    let config = Config::load(env::args().nth(1).as_deref().unwrap_or("config.toml"))
        .expect("Failed to load config");

    // 初始化日志
    logger::init(config.service.log_level);

    // 创建 Tokio 运行时并启动服务器
    tokio::runtime::Runtime::new()
        .expect("Failed to create Tokio runtime")
        .block_on(serve(config))
        .expect("Server encountered an error")
}
