//! 共享的 HTTP 客户端与重试、错误处理逻辑。

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, anyhow};
use ureq::config::RedirectAuthHeaders;
use ureq::http::Response;
use ureq::{Agent, Body};

const MAX_ATTEMPTS: u32 = 4;

pub fn agent() -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_global(Some(Duration::from_secs(300)))
        // Canvas 文件下载会在同一域名内重定向，需要保留认证头；跨域重定向（如 S3）则丢弃
        .redirect_auth_headers(RedirectAuthHeaders::SameHost)
        .build()
        .into()
}

/// 发送请求，遇到限流（429）或服务端临时错误时自动重试。
///
/// `idempotent = false` 的请求（如创建任务）只在服务器明确表示未处理（429/503）时重试，
/// 避免网络中断后重复创建。
pub fn send(
    idempotent: bool,
    mut f: impl FnMut() -> Result<Response<Body>, ureq::Error>,
) -> Result<Response<Body>> {
    let mut backoff = 2u64;
    for attempt in 1..=MAX_ATTEMPTS {
        let last = attempt == MAX_ATTEMPTS;
        match f() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let retryable = if idempotent {
                    matches!(status, 429 | 500 | 502 | 503 | 504)
                } else {
                    matches!(status, 429 | 503)
                };
                if retryable && !last {
                    let wait = retry_after(&resp).unwrap_or(backoff).min(120);
                    log::warn!("服务器返回 HTTP {status}，{wait} 秒后重试（第 {attempt} 次）");
                    sleep(Duration::from_secs(wait));
                    backoff *= 2;
                    continue;
                }
                return Ok(resp);
            }
            Err(e) if idempotent && !last && is_transient(&e) => {
                log::warn!("网络错误：{e}，{backoff} 秒后重试（第 {attempt} 次）");
                sleep(Duration::from_secs(backoff));
                backoff *= 2;
            }
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}

fn retry_after(resp: &Response<Body>) -> Option<u64> {
    resp.headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn is_transient(e: &ureq::Error) -> bool {
    matches!(
        e,
        ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::ConnectionFailed
            | ureq::Error::HostNotFound
            | ureq::Error::BodyStalled
    )
}

/// 非 2xx 状态码转换为带响应正文摘要的错误
pub fn check(mut resp: Response<Body>) -> Result<Response<Body>> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp
        .body_mut()
        .with_config()
        .limit(16 * 1024)
        .lossy_utf8(true)
        .read_to_string()
        .unwrap_or_default();
    Err(anyhow!(
        "HTTP {}: {}",
        status.as_u16(),
        summarize_error(&body)
    ))
}

/// 从 Graph / Canvas / OAuth 的错误 JSON 中提取可读信息
fn summarize_error(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let pick =
            |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        if let Some(e) = v.get("error") {
            if let Some(msg) = pick(e, "message") {
                return match pick(e, "code") {
                    Some(code) => format!("{code}: {msg}"),
                    None => msg,
                };
            }
            if let Some(code) = e.as_str() {
                return match pick(&v, "error_description") {
                    Some(d) => format!("{code}: {}", d.lines().next().unwrap_or("")),
                    None => code.to_string(),
                };
            }
        }
        if let Some(errs) = v.get("errors") {
            return errs.to_string();
        }
    }
    let trimmed: String = body.trim().chars().take(300).collect();
    if trimmed.is_empty() {
        "(无响应内容)".into()
    } else {
        trimmed
    }
}
