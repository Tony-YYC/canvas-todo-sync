//! TOML 配置文件。所有字段都有默认值（与原版 config.yaml 的默认值一致），
//! 配置文件里只需写出想修改的项；拼错的字段名会直接报错。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use jiff::SignedDuration;
use jiff::tz::TimeZone;
use serde::{Deserialize, Deserializer};

use crate::sync::{Policy, Rule};
use crate::text::parse_duration;
use crate::todo::microsoft::auth::DEFAULT_CLIENT_ID;

/// 默认配置模板（`init` 子命令写出的文件）
pub const EXAMPLE: &str = include_str!("../config.example.toml");

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub canvas: CanvasConfig,
    pub microsoft: MicrosoftConfig,
    pub sync: SyncConfig,
    pub assignment: AssignmentConfig,
    pub quiz: QuizConfig,
    pub announcement: TopicConfig,
    pub discussion: TopicConfig,
    pub notification: NotificationConfig,

    /// 配置文件所在目录，用于解析相对路径
    #[serde(skip)]
    pub base_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CanvasConfig {
    pub base_url: String,
    pub token: Option<String>,
    pub token_file: Option<PathBuf>,
    /// 不同步的课程（课程 ID 或课程名）
    pub exclude_courses: Vec<String>,
}

impl Default for CanvasConfig {
    fn default() -> Self {
        Self {
            base_url: "https://oc.sjtu.edu.cn".into(),
            token: None,
            token_file: None,
            exclude_courses: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MicrosoftConfig {
    pub client_id: String,
    /// consumers（个人账户）/ organizations（工作或学校账户）/ common / 租户 ID
    pub tenant: String,
    /// refresh token 存放位置，默认在状态目录下
    pub token_file: Option<PathBuf>,
}

impl Default for MicrosoftConfig {
    fn default() -> Self {
        Self {
            client_id: DEFAULT_CLIENT_ID.into(),
            tenant: "consumers".into(),
            token_file: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListNameMode {
    /// 按项目种类建立列表
    Category,
    /// 按课程建立列表
    Course,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    pub list_name_mode: ListNameMode,
    pub list_names: CategoryListNames,
    pub course_list_template: String,
    pub notification_list_name: String,
    pub ignore_too_old_items: bool,
    pub too_old_days: u32,
    pub ignore_errors: bool,
    pub verbose: bool,
    pub timezone: String,
    pub remove_duplicates: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            list_name_mode: ListNameMode::Category,
            list_names: CategoryListNames::default(),
            course_list_template: "Canvas - {course.name}".into(),
            notification_list_name: "Canvas 通知".into(),
            ignore_too_old_items: false,
            too_old_days: 14,
            ignore_errors: false,
            verbose: true,
            timezone: "Asia/Shanghai".into(),
            remove_duplicates: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CategoryListNames {
    pub assignment: String,
    pub quiz: String,
    pub announcement: String,
    pub discussion: String,
}

impl Default for CategoryListNames {
    fn default() -> Self {
        Self {
            assignment: "Canvas 作业".into(),
            quiz: "Canvas 测验".into(),
            announcement: "Canvas 公告".into(),
            discussion: "Canvas 讨论".into(),
        }
    }
}

/// 待办事项截止日期的来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DueDateMode {
    DueAt,
    LockAt,
}

/// 待办事项提醒时间的来源
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemindMode {
    UnlockAt,
    BeforeDueAt,
    BeforeLockAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDueMode {
    StartAt,
    EndAt,
}

/// 可在 TOML 中写成字符串的时长（`5m`、`1d`、`00:05:00` 等）
#[derive(Debug, Clone, Copy)]
pub struct Duration(pub SignedDuration);

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_duration(&s)
            .map(Duration)
            .map_err(|e| serde::de::Error::custom(format!("{e:#}")))
    }
}

macro_rules! item_config {
    (
        $(#[$meta:meta])*
        $name:ident { $($field:ident : $ty:ty),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Deserialize)]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            pub enabled: bool,
            pub create_content: bool,
            pub create_due_date: bool,
            pub create_remind: bool,
            pub create_importance: bool,
            pub update_title: bool,
            pub update_content: bool,
            pub update_due_date: bool,
            pub update_remind: bool,
            pub update_importance: bool,
            pub set_importance: bool,
            pub title_template: String,
            $(pub $field: $ty,)*
        }

        impl $name {
            fn base_policy(&self) -> Policy {
                Policy {
                    update_title: self.update_title,
                    content: Rule::new(self.create_content, self.update_content),
                    due: Rule::new(self.create_due_date, self.update_due_date),
                    reminder: Rule::new(self.create_remind, self.update_remind),
                    importance: Rule::new(self.create_importance, self.update_importance),
                    important: self.set_importance,
                    checklist: Rule::new(false, false),
                    attachments: false,
                }
            }
        }
    };
}

item_config!(
    /// Canvas 作业
    AssignmentConfig {
        create_attachments: bool,
        create_score_and_commit: bool,
        update_score_and_commit: bool,
        create_comments: bool,
        update_comments: bool,
        due_date_mode: DueDateMode,
        due_date_mode_fallback: bool,
        remind_mode: RemindMode,
        remind_before: Duration,
    }
);

item_config!(
    /// Canvas 测验（经典测验）
    QuizConfig {
        create_attachments: bool,
        create_score_and_commit: bool,
        update_score_and_commit: bool,
        due_date_mode: DueDateMode,
        due_date_mode_fallback: bool,
        remind_mode: RemindMode,
        remind_before: Duration,
    }
);

item_config!(
    /// Canvas 公告 / 讨论
    TopicConfig {
        create_attachments: bool,
        remind_after: Duration,
    }
);

item_config!(
    /// Canvas 全局通知
    NotificationConfig {
        due_date_mode: NotificationDueMode,
        remind_after: Duration,
    }
);

impl Default for AssignmentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            create_content: true,
            create_due_date: true,
            create_remind: true,
            create_importance: false,
            update_title: true,
            update_content: true,
            update_due_date: true,
            update_remind: true,
            update_importance: false,
            set_importance: false,
            title_template: "{course.name} - {assignment.title}".into(),
            create_attachments: true,
            create_score_and_commit: true,
            update_score_and_commit: true,
            create_comments: true,
            update_comments: true,
            due_date_mode: DueDateMode::DueAt,
            due_date_mode_fallback: true,
            remind_mode: RemindMode::BeforeDueAt,
            remind_before: Duration(SignedDuration::from_hours(24)),
        }
    }
}

impl Default for QuizConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            create_content: true,
            create_due_date: true,
            create_remind: true,
            create_importance: false,
            update_title: true,
            update_content: true,
            update_due_date: true,
            update_remind: true,
            update_importance: false,
            set_importance: false,
            title_template: "{course.name} - {quiz.title}".into(),
            create_attachments: true,
            create_score_and_commit: true,
            update_score_and_commit: true,
            due_date_mode: DueDateMode::LockAt,
            due_date_mode_fallback: true,
            remind_mode: RemindMode::BeforeLockAt,
            remind_before: Duration(SignedDuration::from_hours(12)),
        }
    }
}

impl TopicConfig {
    fn defaults(title_template: &str) -> Self {
        Self {
            enabled: true,
            create_content: true,
            create_due_date: true,
            create_remind: true,
            create_importance: false,
            update_title: true,
            update_content: true,
            update_due_date: true,
            // 若为 true，每次运行都会把提醒推迟到“当前时间 + remind_after”
            update_remind: false,
            update_importance: false,
            set_importance: false,
            title_template: title_template.into(),
            create_attachments: true,
            remind_after: Duration(SignedDuration::from_mins(5)),
        }
    }
}

impl Default for TopicConfig {
    fn default() -> Self {
        // 公告与讨论共用此结构，标题模板默认值在 Config::load 中按种类修正
        Self::defaults("")
    }
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            create_content: true,
            create_due_date: true,
            create_remind: true,
            create_importance: false,
            update_title: false,
            update_content: false,
            update_due_date: true,
            update_remind: false,
            update_importance: false,
            set_importance: true,
            title_template: "{notification.title}".into(),
            due_date_mode: NotificationDueMode::StartAt,
            remind_after: Duration(SignedDuration::from_mins(5)),
        }
    }
}

impl AssignmentConfig {
    pub fn policy(&self) -> Policy {
        Policy {
            checklist: Rule::new(self.create_score_and_commit, self.update_score_and_commit),
            attachments: self.create_attachments,
            ..self.base_policy()
        }
    }
}

impl QuizConfig {
    pub fn policy(&self) -> Policy {
        Policy {
            checklist: Rule::new(self.create_score_and_commit, self.update_score_and_commit),
            attachments: self.create_attachments,
            ..self.base_policy()
        }
    }
}

impl TopicConfig {
    pub fn policy(&self) -> Policy {
        Policy {
            attachments: self.create_attachments,
            ..self.base_policy()
        }
    }
}

impl NotificationConfig {
    pub fn policy(&self) -> Policy {
        self.base_policy()
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取配置文件 {}", path.display()))?;
        Self::parse(&raw, path.parent().unwrap_or(Path::new(".")))
            .with_context(|| format!("配置文件 {} 有误", path.display()))
    }

    pub fn parse(raw: &str, base_dir: &Path) -> Result<Self> {
        let mut cfg: Config = toml::from_str(raw)?;
        cfg.base_dir = base_dir.to_path_buf();
        if cfg.announcement.title_template.is_empty() {
            cfg.announcement.title_template = "{course.name} - {announcement.title}".into();
        }
        if cfg.discussion.title_template.is_empty() {
            cfg.discussion.title_template = "{course.name} - {discussion.title}".into();
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        self.timezone()?;
        let url = &self.canvas.base_url;
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            bail!("canvas.base_url 必须以 https:// 开头：{url}");
        }
        if self.microsoft.client_id.trim().is_empty() {
            bail!("microsoft.client_id 不能为空");
        }
        Ok(())
    }

    pub fn timezone(&self) -> Result<TimeZone> {
        TimeZone::get(&self.sync.timezone)
            .with_context(|| format!("无效的时区 sync.timezone = {:?}", self.sync.timezone))
    }

    pub fn canvas_base_url(&self) -> &str {
        self.canvas.base_url.trim_end_matches('/')
    }

    fn resolve(&self, p: &Path) -> PathBuf {
        if let Ok(rest) = p.strip_prefix("~")
            && let Some(home) = std::env::var_os("HOME")
        {
            return PathBuf::from(home).join(rest);
        }
        self.base_dir.join(p)
    }

    /// Canvas 令牌来源优先级：环境变量 CANVAS_TOKEN > canvas.token > canvas.token_file
    pub fn canvas_token(&self) -> Result<String> {
        if let Ok(t) = std::env::var("CANVAS_TOKEN")
            && !t.trim().is_empty()
        {
            return Ok(t.trim().to_string());
        }
        if let Some(t) = &self.canvas.token
            && !t.trim().is_empty()
        {
            return Ok(t.trim().to_string());
        }
        if let Some(p) = &self.canvas.token_file {
            let p = self.resolve(p);
            let t = std::fs::read_to_string(&p)
                .with_context(|| format!("无法读取 Canvas 令牌文件 {}", p.display()))?;
            if !t.trim().is_empty() {
                return Ok(t.trim().to_string());
            }
        }
        bail!(
            "未配置 Canvas 令牌：请设置环境变量 CANVAS_TOKEN，或在配置中设置 canvas.token / canvas.token_file"
        )
    }

    pub fn ms_token_file(&self, state_dir: &Path) -> PathBuf {
        match &self.microsoft.token_file {
            Some(p) => self.resolve(p),
            None => state_dir.join("ms_token.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_matches_defaults() {
        let cfg = Config::parse(EXAMPLE, Path::new("/tmp")).unwrap();
        let def = Config::parse("", Path::new("/tmp")).unwrap();
        // 示例文件与代码默认值必须保持一致
        assert_eq!(format!("{:?}", cfg), format!("{:?}", def));
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(Config::parse("[assignment]\nenable = true", Path::new(".")).is_err());
    }

    #[test]
    fn durations_in_config() {
        let cfg = Config::parse(
            "[assignment]\nremind_before = \"1.00:00:00\"\n[announcement]\nremind_after = \"10m\"",
            Path::new("."),
        )
        .unwrap();
        assert_eq!(
            cfg.assignment.remind_before.0,
            SignedDuration::from_hours(24)
        );
        assert_eq!(
            cfg.announcement.remind_after.0,
            SignedDuration::from_mins(10)
        );
        assert_eq!(
            cfg.announcement.title_template,
            "{course.name} - {announcement.title}"
        );
    }
}
