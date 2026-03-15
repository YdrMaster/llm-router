use colored::{Color, Colorize};
use flexi_logger::DeferredNow;
use log::{Level, LevelFilter, Record};

/// 初始化日志系统
pub(crate) fn init(level: LevelFilter) {
    // <https://docs.rs/flexi_logger/0.30.1/flexi_logger/struct.LogSpecification.html>
    flexi_logger::Logger::try_with_env_or_str(level.to_string())
        .unwrap()
        .format(log_format)
        .start()
        .unwrap();
}

/// 自定义日志格式
fn log_format(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> Result<(), std::io::Error> {
    // 根据日志级别设置颜色
    let color = match record.level() {
        Level::Error => Color::Red,
        Level::Warn => Color::Yellow,
        Level::Info => Color::Green,
        Level::Debug => Color::BrightCyan,
        Level::Trace => Color::BrightBlack,
    };
    write!(
        w,
        "{} {:<5} [{}] {}",
        now.format_rfc3339().color(Color::BrightBlack),
        record.level().to_string().color(color),
        record.module_path().unwrap_or("<unnamed>"),
        record.args().to_string().color(color),
    )
}
