pub mod auth;
mod graph;

pub use graph::GraphTodo;

use anyhow::{Context, Result};
use jiff::Timestamp;

use auth::{OAuth, StoredToken, TokenStore};

/// 用保存的 refresh token 换取 access token，并保存轮换后的新 refresh token
pub fn connect(store: &TokenStore, client_id: &str, tenant: &str) -> Result<GraphTodo> {
    let saved = store.load()?.with_context(|| {
        format!(
            "未找到 Microsoft 令牌（{}），请先运行 `canvas-todo-sync auth login`",
            store.path.display()
        )
    })?;
    if saved.client_id != client_id || saved.tenant != tenant {
        log::warn!(
            "令牌由 client_id={} tenant={} 获取，与当前配置不一致；如刷新失败请重新登录",
            saved.client_id,
            saved.tenant
        );
    }
    let oauth = OAuth::new(&saved.client_id, &saved.tenant);
    let resp = oauth.refresh(&saved.refresh_token)?;
    if let Some(rt) = &resp.refresh_token {
        store
            .save(&StoredToken {
                refresh_token: rt.clone(),
                client_id: saved.client_id,
                tenant: saved.tenant,
                updated_at: Timestamp::now(),
            })
            .context("保存新的 refresh token 失败，请检查文件写入权限")?;
    }
    Ok(GraphTodo::new(&resp.access_token))
}
