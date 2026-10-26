//! 默认文件位置，遵循 XDG 规范，并兼容 systemd 的
//! `ConfigurationDirectory=` / `StateDirectory=`。

use std::env;
use std::path::PathBuf;

pub const APP: &str = env!("CARGO_PKG_NAME");

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(env::var_os("HOME").unwrap_or_else(|| ".".into())).join(fallback),
    }
}

/// systemd 设置的目录变量可能包含多个以冒号分隔的路径，取第一个
fn systemd_dir(var: &str) -> Option<PathBuf> {
    let v = env::var(var).ok()?;
    v.split(':')
        .next()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

pub fn default_config_file() -> PathBuf {
    systemd_dir("CONFIGURATION_DIRECTORY")
        .unwrap_or_else(|| xdg("XDG_CONFIG_HOME", ".config").join(APP))
        .join("config.toml")
}

pub fn default_state_dir() -> PathBuf {
    systemd_dir("STATE_DIRECTORY")
        .unwrap_or_else(|| xdg("XDG_STATE_HOME", ".local/state").join(APP))
}
