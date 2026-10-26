//! Microsoft To Do（Graph API v1.0）实现。

use anyhow::{Context, Result};
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Unit};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ureq::http::Response;
use ureq::{Agent, Body};

use crate::http;
use crate::todo::{ChecklistItem, Link, Task, TaskFields, TaskList, TodoBackend};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// 分片上传的分片大小，必须是 320 KiB 的整数倍
const UPLOAD_CHUNK: usize = 12 * 320 * 1024;

const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

fn seg(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

pub struct GraphTodo {
    agent: Agent,
    auth: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub display_name: Option<String>,
    pub user_principal_name: Option<String>,
    pub mail: Option<String>,
}

#[derive(Deserialize)]
struct Page<T> {
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GList {
    id: String,
    display_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GTask {
    id: String,
    title: Option<String>,
    body: Option<GBody>,
    importance: Option<String>,
    #[serde(default)]
    is_reminder_on: bool,
    reminder_date_time: Option<GDateTime>,
    due_date_time: Option<GDateTime>,
    #[serde(default)]
    linked_resources: Vec<GLink>,
}

#[derive(Deserialize)]
struct GBody {
    content: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GDateTime {
    date_time: String,
    time_zone: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GLink {
    web_url: Option<String>,
    display_name: Option<String>,
    application_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GChecklist {
    id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    is_checked: bool,
}

#[derive(Deserialize)]
struct GAttachment {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSession {
    upload_url: String,
}

impl GDateTime {
    fn civil(&self) -> Option<DateTime> {
        self.date_time.parse().ok()
    }

    fn timestamp(&self) -> Option<Timestamp> {
        let tz = match self.time_zone.as_deref() {
            None | Some("UTC") | Some("") => TimeZone::UTC,
            Some(name) => TimeZone::get(name).unwrap_or(TimeZone::UTC),
        };
        self.civil()?.to_zoned(tz).ok().map(|z| z.timestamp())
    }
}

impl From<GTask> for Task {
    fn from(t: GTask) -> Self {
        Task {
            id: t.id,
            title: t.title,
            body: t.body.and_then(|b| b.content),
            // To Do 的截止日期只有日期部分有意义
            due: t.due_date_time.and_then(|d| d.civil()).map(|d| d.date()),
            reminder: t.reminder_date_time.and_then(|d| d.timestamp()),
            reminder_on: t.is_reminder_on,
            important: t.importance.as_deref() == Some("high"),
            links: t
                .linked_resources
                .into_iter()
                .map(|l| Link {
                    web_url: l.web_url,
                    display_name: l.display_name,
                    application_name: l.application_name,
                })
                .collect(),
        }
    }
}

fn date_json(d: Date) -> Value {
    json!({ "dateTime": format!("{d}T00:00:00.0000000"), "timeZone": "UTC" })
}

fn timestamp_json(t: Timestamp) -> Value {
    let t = t.round(Unit::Second).unwrap_or(t);
    json!({ "dateTime": t.strftime("%Y-%m-%dT%H:%M:%S.0000000").to_string(), "timeZone": "UTC" })
}

fn fields_json(f: &TaskFields) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    if let Some(v) = &f.title {
        m.insert("title".into(), json!(v));
    }
    if let Some(v) = &f.body {
        m.insert(
            "body".into(),
            json!({ "content": v, "contentType": "text" }),
        );
    }
    if let Some(v) = f.due {
        m.insert(
            "dueDateTime".into(),
            v.map(date_json).unwrap_or(Value::Null),
        );
    }
    if let Some(v) = f.reminder {
        m.insert("reminderDateTime".into(), timestamp_json(v));
        m.insert("isReminderOn".into(), json!(true));
    }
    if let Some(v) = f.important {
        m.insert(
            "importance".into(),
            json!(if v { "high" } else { "normal" }),
        );
    }
    m
}

impl GraphTodo {
    pub fn new(access_token: &str) -> Self {
        Self {
            agent: http::agent(),
            auth: format!("Bearer {access_token}"),
        }
    }

    fn task_url(list_id: &str, task_id: &str) -> String {
        format!(
            "{GRAPH}/me/todo/lists/{}/tasks/{}",
            seg(list_id),
            seg(task_id)
        )
    }

    fn get(&self, url: &str) -> Result<Response<Body>> {
        http::check(http::send(true, || {
            self.agent
                .get(url)
                .header("Authorization", &self.auth)
                .call()
        })?)
    }

    fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let mut resp = self.get(url)?;
        Ok(resp
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_json()?)
    }

    /// 获取全部分页（跟随 @odata.nextLink）
    fn get_all<T: DeserializeOwned>(&self, url: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(url.to_string());
        while let Some(u) = next {
            let page: Page<T> = self.get_json(&u)?;
            out.extend(page.value);
            next = page.next_link;
        }
        Ok(out)
    }

    fn post(&self, url: &str, body: &Value) -> Result<Response<Body>> {
        http::check(http::send(false, || {
            self.agent
                .post(url)
                .header("Authorization", &self.auth)
                .send_json(body)
        })?)
    }

    fn patch(&self, url: &str, body: &Value) -> Result<()> {
        http::check(http::send(true, || {
            self.agent
                .patch(url)
                .header("Authorization", &self.auth)
                .send_json(body)
        })?)?;
        Ok(())
    }

    pub fn me(&self) -> Result<Me> {
        self.get_json(&format!(
            "{GRAPH}/me?$select=displayName,userPrincipalName,mail"
        ))
    }
}

impl TodoBackend for GraphTodo {
    fn lists(&self) -> Result<Vec<TaskList>> {
        let lists: Vec<GList> = self
            .get_all(&format!("{GRAPH}/me/todo/lists"))
            .context("获取 To Do 列表失败")?;
        Ok(lists
            .into_iter()
            .map(|l| TaskList {
                id: l.id,
                name: l.display_name,
            })
            .collect())
    }

    fn create_list(&self, name: &str) -> Result<TaskList> {
        let mut resp = self
            .post(
                &format!("{GRAPH}/me/todo/lists"),
                &json!({ "displayName": name }),
            )
            .with_context(|| format!("创建 To Do 列表 {name} 失败"))?;
        let l: GList = resp.body_mut().read_json()?;
        Ok(TaskList {
            id: l.id,
            name: l.display_name,
        })
    }

    fn tasks(&self, list_id: &str) -> Result<Vec<Task>> {
        let tasks: Vec<GTask> = self
            .get_all(&format!(
                "{GRAPH}/me/todo/lists/{}/tasks?$expand=linkedResources",
                seg(list_id)
            ))
            .context("获取 To Do 任务失败")?;
        Ok(tasks.into_iter().map(Task::from).collect())
    }

    fn create_task(&self, list_id: &str, fields: &TaskFields, link: &Link) -> Result<Task> {
        let mut body = fields_json(fields);
        let mut l = serde_json::Map::new();
        if let Some(v) = &link.web_url {
            l.insert("webUrl".into(), json!(v));
        }
        if let Some(v) = &link.display_name {
            l.insert("displayName".into(), json!(v));
        }
        if let Some(v) = &link.application_name {
            l.insert("applicationName".into(), json!(v));
        }
        body.insert("linkedResources".into(), json!([l]));
        let mut resp = self
            .post(
                &format!("{GRAPH}/me/todo/lists/{}/tasks", seg(list_id)),
                &Value::Object(body),
            )
            .context("创建 To Do 任务失败")?;
        let t: GTask = resp.body_mut().read_json()?;
        let mut task = Task::from(t);
        if task.links.is_empty() {
            task.links.push(link.clone());
        }
        Ok(task)
    }

    fn update_task(&self, list_id: &str, task_id: &str, fields: &TaskFields) -> Result<()> {
        self.patch(
            &Self::task_url(list_id, task_id),
            &Value::Object(fields_json(fields)),
        )
        .context("更新 To Do 任务失败")
    }

    fn delete_task(&self, list_id: &str, task_id: &str) -> Result<()> {
        let url = Self::task_url(list_id, task_id);
        http::check(http::send(true, || {
            self.agent
                .delete(&url)
                .header("Authorization", &self.auth)
                .call()
        })?)
        .context("删除 To Do 任务失败")?;
        Ok(())
    }

    fn checklist(&self, list_id: &str, task_id: &str) -> Result<Vec<ChecklistItem>> {
        let items: Vec<GChecklist> = self
            .get_all(&format!(
                "{}/checklistItems",
                Self::task_url(list_id, task_id)
            ))
            .context("获取任务步骤失败")?;
        Ok(items
            .into_iter()
            .map(|c| ChecklistItem {
                id: c.id,
                text: c.display_name,
                checked: c.is_checked,
            })
            .collect())
    }

    fn add_checklist_item(
        &self,
        list_id: &str,
        task_id: &str,
        text: &str,
        checked: bool,
    ) -> Result<()> {
        self.post(
            &format!("{}/checklistItems", Self::task_url(list_id, task_id)),
            &json!({ "displayName": text, "isChecked": checked }),
        )
        .context("添加任务步骤失败")?;
        Ok(())
    }

    fn update_checklist_item(
        &self,
        list_id: &str,
        task_id: &str,
        item_id: &str,
        text: Option<&str>,
        checked: bool,
    ) -> Result<()> {
        let mut body = json!({ "isChecked": checked });
        if let Some(t) = text {
            body["displayName"] = json!(t);
        }
        self.patch(
            &format!(
                "{}/checklistItems/{}",
                Self::task_url(list_id, task_id),
                seg(item_id)
            ),
            &body,
        )
        .context("更新任务步骤失败")
    }

    fn attachment_names(&self, list_id: &str, task_id: &str) -> Result<Vec<String>> {
        let items: Vec<GAttachment> = self
            .get_all(&format!(
                "{}/attachments?$select=id,name",
                Self::task_url(list_id, task_id)
            ))
            .context("获取任务附件失败")?;
        Ok(items.into_iter().filter_map(|a| a.name).collect())
    }

    fn upload_attachment(
        &self,
        list_id: &str,
        task_id: &str,
        name: &str,
        data: &[u8],
    ) -> Result<()> {
        let mut resp = self
            .post(
                &format!(
                    "{}/attachments/createUploadSession",
                    Self::task_url(list_id, task_id)
                ),
                &json!({ "attachmentInfo": {
                    "attachmentType": "file",
                    "name": name,
                    "size": data.len(),
                }}),
            )
            .context("创建附件上传会话失败")?;
        let mut session: UploadSession = resp.body_mut().read_json()?;
        // v1.0 返回的 uploadUrl 缺少 /content 段，直接 PUT 会 404
        if !session.upload_url.ends_with("/content") {
            session.upload_url.push_str("/content");
        }
        let total = data.len();
        // To Do 的上传地址位于 graph.microsoft.com，需要携带令牌（与 OneDrive 的预授权地址不同）；
        // 只对 Graph 域名附带，防止令牌发往其他地址
        let auth = session
            .upload_url
            .starts_with("https://graph.microsoft.com/")
            .then_some(self.auth.as_str());
        for (i, chunk) in data.chunks(UPLOAD_CHUNK).enumerate() {
            let start = i * UPLOAD_CHUNK;
            let range = format!("bytes {start}-{}/{total}", start + chunk.len() - 1);
            http::check(http::send(true, || {
                let mut req = self
                    .agent
                    .put(&session.upload_url)
                    .header("Content-Range", &range)
                    .header("Content-Type", "application/octet-stream");
                if let Some(a) = auth {
                    req = req.header("Authorization", a);
                }
                req.send(chunk)
            })?)
            .with_context(|| format!("上传附件分片失败（{range}）"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_graph_datetimes() {
        let d = GDateTime {
            date_time: "2022-10-20T00:00:00.0000000".into(),
            time_zone: Some("UTC".into()),
        };
        assert_eq!(d.civil().unwrap().date(), jiff::civil::date(2022, 10, 20));
        let r = GDateTime {
            date_time: "2022-10-19T15:59:59.0000000".into(),
            time_zone: Some("UTC".into()),
        };
        assert_eq!(
            r.timestamp().unwrap(),
            "2022-10-19T15:59:59Z".parse::<Timestamp>().unwrap()
        );
    }

    #[test]
    fn serializes_fields() {
        let f = TaskFields {
            due: Some(Some(jiff::civil::date(2022, 10, 20))),
            reminder: Some("2022-10-19T15:59:59.5Z".parse().unwrap()),
            ..Default::default()
        };
        let v = Value::Object(fields_json(&f));
        assert_eq!(v["dueDateTime"]["dateTime"], "2022-10-20T00:00:00.0000000");
        assert_eq!(
            v["reminderDateTime"]["dateTime"],
            "2022-10-19T16:00:00.0000000"
        );
        assert_eq!(v["isReminderOn"], true);
        let clear = TaskFields {
            due: Some(None),
            ..Default::default()
        };
        assert!(Value::Object(fields_json(&clear))["dueDateTime"].is_null());
    }
}
