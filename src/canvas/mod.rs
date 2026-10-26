//! Canvas LMS REST API 客户端。

pub mod models;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use ureq::http::Response;
use ureq::{Agent, Body};

use crate::http;
use models::*;

pub struct CanvasClient {
    agent: Agent,
    base: String,
    token: String,
}

impl CanvasClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            agent: http::agent(),
            base: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    /// 把相对路径补全为绝对 URL
    pub fn absolute_url(&self, url: &str) -> String {
        if url.starts_with("https://") || url.starts_with("http://") {
            url.to_string()
        } else if url.starts_with('/') {
            format!("{}{url}", self.base)
        } else {
            format!("{}/{url}", self.base)
        }
    }

    /// 只对 Canvas 自身域名附带令牌，防止泄露给第三方
    fn is_own(&self, url: &str) -> bool {
        url == self.base || url.starts_with(&format!("{}/", self.base))
    }

    fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<Response<Body>> {
        let url = self.absolute_url(url);
        let auth = self.is_own(&url).then(|| format!("Bearer {}", self.token));
        http::send(true, || {
            let mut req = self.agent.get(&url);
            if let Some(a) = &auth {
                req = req.header("Authorization", a);
            }
            for (k, v) in query {
                req = req.query(*k, *v);
            }
            req.call()
        })
        .with_context(|| format!("请求 Canvas 失败：{url}"))
    }

    /// GET 并解析 JSON；401/403/404 返回 None（如课程关闭了公告/讨论栏目）
    fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Option<T>> {
        let resp = self.get(path, query)?;
        if matches!(resp.status().as_u16(), 401 | 403 | 404) {
            log::debug!("Canvas {path} 返回 HTTP {}，跳过", resp.status().as_u16());
            return Ok(None);
        }
        let mut resp = http::check(resp).with_context(|| format!("Canvas 请求失败：{path}"))?;
        let v = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_json()
            .with_context(|| format!("解析 Canvas 响应失败：{path}"))?;
        Ok(Some(v))
    }

    /// 获取全部分页结果（跟随 Link 头中的 rel="next"）
    fn get_all<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<T>> {
        let mut q = query.to_vec();
        q.push(("per_page", "100"));
        let mut out = Vec::new();
        let mut next: Option<String> = None;
        let mut first = true;
        loop {
            let resp = if first {
                self.get(path, &q)?
            } else {
                match next.take() {
                    Some(url) => self.get(&url, &[])?,
                    None => break,
                }
            };
            first = false;
            if matches!(resp.status().as_u16(), 401 | 403 | 404) {
                log::debug!("Canvas {path} 返回 HTTP {}，跳过", resp.status().as_u16());
                break;
            }
            let mut resp = http::check(resp).with_context(|| format!("Canvas 请求失败：{path}"))?;
            next = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(next_link)
                .filter(|u| self.is_own(u));
            let page: Vec<T> = resp
                .body_mut()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_json()
                .with_context(|| format!("解析 Canvas 响应失败：{path}"))?;
            out.extend(page);
            if next.is_none() {
                break;
            }
        }
        Ok(out)
    }

    pub fn profile(&self) -> Result<UserProfile> {
        let resp = self.get("/api/v1/users/self/profile", &[])?;
        if resp.status().as_u16() == 401 {
            bail!("Canvas 令牌无效（HTTP 401）");
        }
        let mut resp = http::check(resp)?;
        Ok(resp.body_mut().read_json()?)
    }

    pub fn courses(&self) -> Result<Vec<Course>> {
        self.get_all("/api/v1/courses", &[("enrollment_state", "active")])
    }

    pub fn assignments(&self, course_id: i64) -> Result<Vec<Assignment>> {
        self.get_all(&format!("/api/v1/courses/{course_id}/assignments"), &[])
    }

    pub fn announcements(&self, course_id: i64) -> Result<Vec<Topic>> {
        self.get_all(
            &format!("/api/v1/courses/{course_id}/discussion_topics"),
            &[("only_announcements", "true"), ("filter_by", "all")],
        )
    }

    pub fn discussions(&self, course_id: i64) -> Result<Vec<Topic>> {
        self.get_all(
            &format!("/api/v1/courses/{course_id}/discussion_topics"),
            &[],
        )
    }

    pub fn submission(&self, course_id: i64, assignment_id: i64) -> Result<Option<Submission>> {
        self.get_json(
            &format!("/api/v1/courses/{course_id}/assignments/{assignment_id}/submissions/self"),
            &[("include[]", "submission_comments")],
        )
    }

    pub fn quiz_submissions(&self, course_id: i64, quiz_id: i64) -> Result<Vec<QuizSubmission>> {
        Ok(self
            .get_json::<QuizSubmissions>(
                &format!("/api/v1/courses/{course_id}/quizzes/{quiz_id}/submissions"),
                &[("per_page", "100")],
            )?
            .map(|d| d.quiz_submissions)
            .unwrap_or_default())
    }

    pub fn notifications(&self) -> Result<Vec<Notification>> {
        Ok(self
            .get_json("/api/v1/accounts/self/account_notifications", &[])?
            .unwrap_or_default())
    }

    /// 下载文件；超过 `max_bytes` 时返回 None
    pub fn download(&self, url: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        let mut resp =
            http::check(self.get(url, &[])?).with_context(|| format!("下载文件失败：{url}"))?;
        if resp.body().content_length().is_some_and(|n| n > max_bytes) {
            return Ok(None);
        }
        match resp.body_mut().with_config().limit(max_bytes).read_to_vec() {
            Ok(data) => Ok(Some(data)),
            Err(ureq::Error::BodyExceedsLimit(_)) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("下载文件失败：{url}")),
        }
    }
}

/// 解析 `Link: <url>; rel="current", <url>; rel="next", ...`
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim() == r#"rel="next""#)
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string()
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_header() {
        let h = r#"<https://oc.sjtu.edu.cn/api/v1/courses?page=1&per_page=10>; rel="current",<https://oc.sjtu.edu.cn/api/v1/courses?page=12&per_page=10>; rel="next",<https://oc.sjtu.edu.cn/api/v1/courses?page=1&per_page=10>; rel="first""#;
        assert_eq!(
            next_link(h).as_deref(),
            Some("https://oc.sjtu.edu.cn/api/v1/courses?page=12&per_page=10")
        );
        assert_eq!(next_link(r#"<https://x/a>; rel="last""#), None);
    }
}
