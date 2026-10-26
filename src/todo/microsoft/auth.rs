//! Microsoft 账户 OAuth 2.0 认证。
//!
//! - 授权码 + PKCE（默认）：在任意设备的浏览器中打开授权链接，登录后把跳转到的
//!   空白页地址粘贴回终端。适用于原版的公共 client_id，无需自己注册应用。
//! - 设备代码流（`--device`）：在任意设备打开 microsoft.com/devicelogin 输入代码即可，
//!   需要使用自己注册并开启了“允许公共客户端流”的应用。
//!
//! 获得的 refresh token 保存在本地文件中，每次运行时自动刷新并轮换。

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::Timestamp;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ureq::Agent;

use crate::http;

/// 原版 TodoSync 注册的公共客户端（MyTodoApp）
pub const DEFAULT_CLIENT_ID: &str = "49694ef2-8751-4ac9-8431-8817c27350b4";
pub const SCOPES: &str = "Tasks.ReadWrite User.Read offline_access";
pub const NATIVE_REDIRECT: &str = "https://login.microsoftonline.com/common/oauth2/nativeclient";

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredToken {
    pub refresh_token: String,
    pub client_id: String,
    pub tenant: String,
    pub updated_at: Timestamp,
}

pub struct TokenStore {
    pub path: PathBuf,
}

impl TokenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<Option<StoredToken>> {
        match fs::read_to_string(&self.path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| {
                format!("令牌文件格式错误：{}", self.path.display())
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("无法读取令牌文件 {}", self.path.display())),
        }
    }

    /// 原子写入（临时文件 + rename），权限 0600
    pub fn save(&self, token: &StoredToken) -> Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).with_context(|| format!("无法创建目录 {}", dir.display()))?;
        let tmp = self.path.with_extension("json.tmp");
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .with_context(|| format!("无法写入 {}", tmp.display()))?;
            f.write_all(serde_json::to_string_pretty(token)?.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("无法写入令牌文件 {}", self.path.display()))?;
        Ok(())
    }

    pub fn delete(&self) -> Result<bool> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: String,
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    message: String,
    #[serde(default = "default_interval")]
    interval: u64,
    expires_in: u64,
}

fn default_interval() -> u64 {
    5
}

pub struct OAuth {
    agent: Agent,
    pub client_id: String,
    pub tenant: String,
}

/// RFC 3986 非保留字符之外的全部编码
const QUERY: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn enc(s: &str) -> String {
    utf8_percent_encode(s, QUERY).to_string()
}

fn random_b64(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("系统随机数不可用");
    URL_SAFE_NO_PAD.encode(buf)
}

impl OAuth {
    pub fn new(client_id: &str, tenant: &str) -> Self {
        Self {
            agent: http::agent(),
            client_id: client_id.to_string(),
            tenant: tenant.to_string(),
        }
    }

    fn endpoint(&self, name: &str) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/{name}",
            self.tenant
        )
    }

    /// POST 表单到 token 端点。成功返回 Ok(Ok(token))，OAuth 错误返回 Ok(Err((code, desc)))
    fn token_request(
        &self,
        form: &[(&str, &str)],
    ) -> Result<Result<TokenResponse, (String, String)>> {
        let url = self.endpoint("token");
        let mut resp = http::send(false, || {
            self.agent.post(&url).send_form(form.iter().copied())
        })
        .context("请求 Microsoft 令牌端点失败")?;
        let body = resp.body_mut().read_to_string()?;
        if resp.status().is_success() {
            return Ok(Ok(serde_json::from_str(&body).context("解析令牌响应失败")?));
        }
        match serde_json::from_str::<OAuthError>(&body) {
            Ok(e) => Ok(Err((e.error, e.error_description))),
            Err(_) => bail!("令牌端点返回 HTTP {}：{body}", resp.status().as_u16()),
        }
    }

    pub fn refresh(&self, refresh_token: &str) -> Result<TokenResponse> {
        match self.token_request(&[
            ("client_id", &self.client_id),
            ("scope", SCOPES),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])? {
            Ok(t) => Ok(t),
            Err((code, desc)) => {
                let hint = if code == "invalid_grant" {
                    "\n令牌已失效或被撤销，请重新运行 `canvas-todo-sync auth login`"
                } else {
                    ""
                };
                Err(anyhow!(
                    "刷新 Microsoft 令牌失败：{code}: {}{hint}",
                    first_line(&desc)
                ))
            }
        }
    }

    /// 授权码 + PKCE，用户手动粘贴回调地址
    pub fn login_with_browser(&self) -> Result<TokenResponse> {
        let verifier = random_b64(32);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_b64(12);
        let url = format!(
            "{}?client_id={}&response_type=code&redirect_uri={}&response_mode=query&scope={}&state={}&code_challenge={}&code_challenge_method=S256&prompt=select_account",
            self.endpoint("authorize"),
            enc(&self.client_id),
            enc(NATIVE_REDIRECT),
            enc(SCOPES),
            enc(&state),
            challenge,
        );
        eprintln!("1. 在任意设备的浏览器中打开以下链接，登录 Microsoft 账户并同意授权：\n");
        eprintln!("{url}\n");
        eprintln!("2. 授权完成后浏览器会跳转到一个空白页面，地址形如");
        eprintln!("   {NATIVE_REDIRECT}?code=M.C5xx_...&state=...");
        eprintln!("3. 复制该页面的完整地址，粘贴到下面并回车（授权码几分钟内有效）：\n");
        eprint!("> ");
        std::io::stderr().flush().ok();

        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let input = line.trim();
        if input.is_empty() {
            bail!("未输入任何内容");
        }
        let (code, returned_state) = parse_redirect(input)?;
        if let Some(s) = returned_state
            && s != state
        {
            bail!("state 不匹配，请确认粘贴的是本次授权的地址");
        }
        match self.token_request(&[
            ("client_id", &self.client_id),
            ("scope", SCOPES),
            ("code", &code),
            ("redirect_uri", NATIVE_REDIRECT),
            ("grant_type", "authorization_code"),
            ("code_verifier", &verifier),
        ])? {
            Ok(t) => Ok(t),
            Err((code, desc)) => bail!("获取令牌失败：{code}: {}", first_line(&desc)),
        }
    }

    /// 设备代码流
    pub fn login_with_device_code(&self) -> Result<TokenResponse> {
        let url = self.endpoint("devicecode");
        let mut resp = http::send(false, || {
            self.agent
                .post(&url)
                .send_form([("client_id", self.client_id.as_str()), ("scope", SCOPES)])
        })?;
        let body = resp.body_mut().read_to_string()?;
        if !resp.status().is_success() {
            let msg = serde_json::from_str::<OAuthError>(&body)
                .map(|e| format!("{}: {}", e.error, first_line(&e.error_description)))
                .unwrap_or(body);
            let hint = if msg.contains("AADSTS70002") || msg.contains("AADSTS7000218") {
                "\n该 client_id 未启用“允许公共客户端流”，无法使用设备代码登录。\
                 请改用 `auth login`（浏览器方式），或在自己注册的应用中开启此选项。"
            } else {
                ""
            };
            bail!("申请设备代码失败：{msg}{hint}");
        }
        let dc: DeviceCode = serde_json::from_str(&body)?;
        eprintln!("{}\n", dc.message);
        eprintln!("等待授权中...");

        let mut interval = dc.interval.max(1);
        let deadline = std::time::Instant::now() + Duration::from_secs(dc.expires_in);
        while std::time::Instant::now() < deadline {
            sleep(Duration::from_secs(interval));
            match self.token_request(&[
                ("client_id", &self.client_id),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &dc.device_code),
            ])? {
                Ok(t) => return Ok(t),
                Err((code, _)) if code == "authorization_pending" => {}
                Err((code, _)) if code == "slow_down" => interval += 5,
                Err((code, desc)) => bail!("设备代码登录失败：{code}: {}", first_line(&desc)),
            }
        }
        bail!("设备代码已过期，请重新运行")
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// 从粘贴的回调地址（或单独的授权码）中提取 code 和 state
fn parse_redirect(input: &str) -> Result<(String, Option<String>)> {
    let Some((_, query)) = input.split_once('?') else {
        // 用户可能只粘贴了 code
        return Ok((input.to_string(), None));
    };
    let query = query.split('#').next().unwrap_or("");
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_desc = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = percent_encoding::percent_decode_str(&v.replace('+', " "))
            .decode_utf8_lossy()
            .into_owned();
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            "error_description" => error_desc = Some(v),
            _ => {}
        }
    }
    if let Some(e) = error {
        bail!("授权失败：{e}: {}", error_desc.unwrap_or_default());
    }
    let code = code.ok_or_else(|| anyhow!("地址中没有找到 code 参数"))?;
    Ok((code, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_parsing() {
        let (c, s) = parse_redirect(
            "https://login.microsoftonline.com/common/oauth2/nativeclient?code=M.C5_BAY.2.U.abc%21&state=xyz",
        )
        .unwrap();
        assert_eq!(c, "M.C5_BAY.2.U.abc!");
        assert_eq!(s.as_deref(), Some("xyz"));
        assert_eq!(parse_redirect("M.C5_raw").unwrap().0, "M.C5_raw");
        assert!(parse_redirect("https://x/?error=access_denied&error_description=no").is_err());
    }
}
