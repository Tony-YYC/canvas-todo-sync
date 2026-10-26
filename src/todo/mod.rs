//! 待办服务的抽象层。同步引擎只依赖 [`TodoBackend`] trait，
//! 以后接入滴答清单等服务时只需新增一个实现。

pub mod microsoft;

use anyhow::Result;
use jiff::Timestamp;
use jiff::civil::Date;

#[derive(Debug, Clone)]
pub struct TaskList {
    pub id: String,
    pub name: String,
}

/// 关联链接。同步程序用第一个关联链接作为任务的唯一标识（与原版兼容）
#[derive(Debug, Clone, Default)]
pub struct Link {
    pub web_url: Option<String>,
    pub display_name: Option<String>,
    pub application_name: Option<String>,
}

impl Link {
    /// 去重用的键：有绝对 URL 时用 URL，否则用 `id:<显示名>`
    pub fn key(&self) -> Option<String> {
        match (&self.web_url, &self.display_name) {
            (Some(u), _) if u.starts_with("https://") || u.starts_with("http://") => {
                Some(u.clone())
            }
            (_, Some(n)) if !n.is_empty() => Some(format!("id:{n}")),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub due: Option<Date>,
    pub reminder: Option<Timestamp>,
    pub reminder_on: bool,
    pub important: bool,
    pub links: Vec<Link>,
}

/// 创建任务时的全部字段，或更新任务时需要修改的字段（None 表示不修改）
#[derive(Debug, Clone, Default)]
pub struct TaskFields {
    pub title: Option<String>,
    pub body: Option<String>,
    /// `Some(None)` 表示清除截止日期
    pub due: Option<Option<Date>>,
    /// 设置提醒时间并开启提醒
    pub reminder: Option<Timestamp>,
    pub important: Option<bool>,
}

impl TaskFields {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.body.is_none()
            && self.due.is_none()
            && self.reminder.is_none()
            && self.important.is_none()
    }

    /// 变更字段名，用于日志
    pub fn describe(&self) -> String {
        let mut v = Vec::new();
        if self.title.is_some() {
            v.push("标题");
        }
        if self.body.is_some() {
            v.push("备注");
        }
        if self.due.is_some() {
            v.push("截止日期");
        }
        if self.reminder.is_some() {
            v.push("提醒");
        }
        if self.important.is_some() {
            v.push("重要性");
        }
        v.join("、")
    }

    /// 把已提交的修改应用到本地缓存的任务上
    pub fn apply_to(&self, t: &mut Task) {
        if let Some(v) = &self.title {
            t.title = Some(v.clone());
        }
        if let Some(v) = &self.body {
            t.body = Some(v.clone());
        }
        if let Some(v) = self.due {
            t.due = v;
        }
        if let Some(v) = self.reminder {
            t.reminder = Some(v);
            t.reminder_on = true;
        }
        if let Some(v) = self.important {
            t.important = v;
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChecklistItem {
    pub id: String,
    pub text: String,
    pub checked: bool,
}

pub trait TodoBackend {
    fn lists(&self) -> Result<Vec<TaskList>>;
    fn create_list(&self, name: &str) -> Result<TaskList>;

    fn tasks(&self, list_id: &str) -> Result<Vec<Task>>;
    /// 创建任务并同时写入关联链接
    fn create_task(&self, list_id: &str, fields: &TaskFields, link: &Link) -> Result<Task>;
    fn update_task(&self, list_id: &str, task_id: &str, fields: &TaskFields) -> Result<()>;
    fn delete_task(&self, list_id: &str, task_id: &str) -> Result<()>;

    fn checklist(&self, list_id: &str, task_id: &str) -> Result<Vec<ChecklistItem>>;
    fn add_checklist_item(
        &self,
        list_id: &str,
        task_id: &str,
        text: &str,
        checked: bool,
    ) -> Result<()>;
    /// `text = None` 表示只修改勾选状态
    fn update_checklist_item(
        &self,
        list_id: &str,
        task_id: &str,
        item_id: &str,
        text: Option<&str>,
        checked: bool,
    ) -> Result<()>;

    fn attachment_names(&self, list_id: &str, task_id: &str) -> Result<Vec<String>>;
    fn upload_attachment(
        &self,
        list_id: &str,
        task_id: &str,
        name: &str,
        data: &[u8],
    ) -> Result<()>;
}
