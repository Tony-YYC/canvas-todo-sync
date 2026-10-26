//! 极简日志实现：输出到 stderr。
//! 在 systemd 下运行时（存在 JOURNAL_STREAM）使用 `<N>` 优先级前缀且不带时间戳，
//! 由 journald 记录时间并支持 `journalctl -p` 按级别过滤。

use log::{Level, LevelFilter, Log, Metadata, Record};

struct Logger {
    journal: bool,
}

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.target().starts_with(env!("CARGO_CRATE_NAME"))
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        if self.journal {
            let prio = match r.level() {
                Level::Error => 3,
                Level::Warn => 4,
                Level::Info => 6,
                Level::Debug | Level::Trace => 7,
            };
            eprintln!("<{prio}>{}", r.args());
        } else {
            let now = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S");
            let level = match r.level() {
                Level::Error => "错误",
                Level::Warn => "警告",
                Level::Info => "信息",
                Level::Debug | Level::Trace => "调试",
            };
            eprintln!("{now} [{level}] {}", r.args());
        }
    }

    fn flush(&self) {}
}

pub fn init(level: LevelFilter) {
    let journal = std::env::var_os("JOURNAL_STREAM").is_some();
    log::set_logger(Box::leak(Box::new(Logger { journal }))).expect("logger 只能初始化一次");
    log::set_max_level(level);
}
