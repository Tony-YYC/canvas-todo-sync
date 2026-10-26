//! Canvas 来源：遍历课程，把作业、测验、公告、讨论、全局通知转换为 [`SyncItem`]。

use std::collections::HashMap;

use anyhow::Result;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use super::{ATTACHMENT_LIMIT, AttachmentFetcher, ChecklistEntry, Reconciler, Rule, SyncItem};
use crate::canvas::CanvasClient;
use crate::canvas::models::{Assignment, Course, Notification, QuizSubmission, Submission, Topic};
use crate::config::{
    Config, DueDateMode, ListNameMode, NotificationDueMode, RemindMode, TopicConfig,
};
use crate::text::html::html_to_text;
use crate::text::{AttachmentRef, course_code_short, extract_attachments, fmt_num, render};
use crate::todo::Link;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Assignment,
    Quiz,
    Announcement,
    Discussion,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Assignment => "作业",
            Kind::Quiz => "测验",
            Kind::Announcement => "公告",
            Kind::Discussion => "讨论",
        }
    }
}

struct Fetcher<'a>(&'a CanvasClient);

impl AttachmentFetcher for Fetcher<'_> {
    fn fetch(&self, url: &str) -> Result<Option<Vec<u8>>> {
        self.0.download(&self.0.absolute_url(url), ATTACHMENT_LIMIT)
    }
}

pub struct CanvasSync<'a> {
    cfg: &'a Config,
    canvas: &'a CanvasClient,
    tz: TimeZone,
    now: Timestamp,
    too_old_before: Option<Timestamp>,
    category_lists: HashMap<Kind, String>,
    course_lists: HashMap<i64, String>,
    pub courses: usize,
    pub errors: usize,
}

fn course_vars(c: &Course) -> Vec<(&'static str, String)> {
    vec![
        ("course.name", c.name.clone()),
        ("course.coursecode", c.course_code.clone()),
        ("course.coursecodeshort", course_code_short(&c.course_code)),
        (
            "course.originalname",
            c.original_name.clone().unwrap_or_else(|| c.name.clone()),
        ),
    ]
}

fn render_with(template: &str, course: Option<&Course>, extra: &[(&str, &str)]) -> String {
    let vars = course.map(course_vars).unwrap_or_default();
    let mut all: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    all.extend_from_slice(extra);
    render(template, &all)
}

fn fmt_time(t: Timestamp, tz: &TimeZone) -> String {
    t.to_zoned(tz.clone())
        .strftime("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// 与原版一致：包含“未”或“正在”的描述视为未完成
fn entry(text: String) -> ChecklistEntry {
    let checked = !text.contains('未') && !text.contains("正在");
    ChecklistEntry { text, checked }
}

fn submission_entries(
    s: &Submission,
    points: Option<f64>,
    with_comments: bool,
    tz: &TimeZone,
) -> Vec<ChecklistEntry> {
    let submitted = match s.submitted_at {
        Some(t) => format!("已提交（提交时间：{}）", fmt_time(t, tz)),
        None => "未提交".into(),
    };
    let graded = match (&s.grade, s.graded_at) {
        (Some(g), Some(t)) => format!(
            "已评分：{g}/{}（评分时间：{}）",
            fmt_num(points.unwrap_or(0.0)),
            fmt_time(t, tz)
        ),
        (Some(g), None) => format!("已评分：{g}/{}", fmt_num(points.unwrap_or(0.0))),
        (None, _) => "未评分".into(),
    };
    let mut v = vec![entry(submitted), entry(graded)];
    if with_comments {
        v.extend(s.submission_comments.iter().map(|c| ChecklistEntry {
            text: format!("{} 评论：{}", c.author_name, c.comment),
            checked: true,
        }));
    }
    v
}

fn quiz_entry(q: &QuizSubmission, tz: &TimeZone) -> ChecklistEntry {
    let score = || {
        format!(
            "尝试 {}：{}/{}（提交时间：{}）",
            q.attempt.unwrap_or(0),
            q.score.map(fmt_num).unwrap_or_default(),
            q.quiz_points_possible.map(fmt_num).unwrap_or_default(),
            q.finished_at.map(|t| fmt_time(t, tz)).unwrap_or_default()
        )
    };
    entry(match q.workflow_state.as_str() {
        "untaken" => format!(
            "正在进行测验（开始时间：{}）",
            q.started_at.map(|t| fmt_time(t, tz)).unwrap_or_default()
        ),
        "complete" => score(),
        "pending_review" => format!("{}（*有些问题尚未计分）", score()),
        _ => "无法识别的尝试".into(),
    })
}

fn pick_due(mode: DueDateMode, fallback: bool, a: &Assignment) -> Option<Timestamp> {
    let (primary, secondary) = match mode {
        DueDateMode::DueAt => (a.due_at, a.lock_at),
        DueDateMode::LockAt => (a.lock_at, a.due_at),
    };
    if fallback {
        primary.or(secondary)
    } else {
        primary
    }
}

fn pick_remind(mode: RemindMode, before: SignedDuration, a: &Assignment) -> Option<Timestamp> {
    match mode {
        RemindMode::UnlockAt => a.unlock_at,
        RemindMode::BeforeDueAt => a.due_at.and_then(|t| t.checked_sub(before).ok()),
        RemindMode::BeforeLockAt => a.lock_at.and_then(|t| t.checked_sub(before).ok()),
    }
}

impl<'a> CanvasSync<'a> {
    pub fn new(cfg: &'a Config, canvas: &'a CanvasClient) -> Result<Self> {
        let now = Timestamp::now();
        let too_old_before = cfg.sync.ignore_too_old_items.then(|| {
            now.checked_sub(SignedDuration::from_hours(
                24 * i64::from(cfg.sync.too_old_days),
            ))
            .unwrap_or(now)
        });
        Ok(Self {
            cfg,
            canvas,
            tz: cfg.timezone()?,
            now,
            too_old_before,
            category_lists: HashMap::new(),
            course_lists: HashMap::new(),
            courses: 0,
            errors: 0,
        })
    }

    fn date(&self, t: Timestamp) -> Date {
        t.to_zoned(self.tz.clone()).date()
    }

    fn too_old(&self, t: Option<Timestamp>) -> bool {
        matches!((self.too_old_before, t), (Some(limit), Some(t)) if t < limit)
    }

    fn after_now(&self, d: SignedDuration) -> Option<Timestamp> {
        self.now.checked_add(d).ok()
    }

    fn excluded(&self, c: &Course) -> bool {
        self.cfg
            .canvas
            .exclude_courses
            .iter()
            .any(|x| *x == c.id.to_string() || *x == c.name || *x == c.course_code)
    }

    /// 执行一次完整同步。读取课程、列表、任务失败视为致命错误；单个条目失败只记录。
    pub fn run(&mut self, rec: &mut Reconciler<'_>) -> Result<()> {
        let cfg = self.cfg;
        log::info!("读取 Canvas 课程列表");
        let courses: Vec<Course> = self
            .canvas
            .courses()?
            .into_iter()
            .filter(|c| {
                c.enrollments
                    .iter()
                    .any(|e| e.kind.as_deref() == Some("student"))
            })
            .filter(|c| !self.excluded(c))
            .collect();
        log::info!("共 {} 门课程", courses.len());

        log::info!("检查 To Do 列表");
        let notification_list = if cfg.notification.enabled {
            Some(rec.ensure_list(&cfg.sync.notification_list_name)?)
        } else {
            None
        };
        let kinds = [
            (
                Kind::Assignment,
                cfg.assignment.enabled,
                &cfg.sync.list_names.assignment,
            ),
            (Kind::Quiz, cfg.quiz.enabled, &cfg.sync.list_names.quiz),
            (
                Kind::Announcement,
                cfg.announcement.enabled,
                &cfg.sync.list_names.announcement,
            ),
            (
                Kind::Discussion,
                cfg.discussion.enabled,
                &cfg.sync.list_names.discussion,
            ),
        ];
        match cfg.sync.list_name_mode {
            ListNameMode::Category => {
                for (kind, enabled, name) in kinds {
                    if enabled {
                        self.category_lists.insert(kind, rec.ensure_list(name)?);
                    }
                }
            }
            ListNameMode::Course => {
                if kinds.iter().any(|k| k.1) {
                    for c in &courses {
                        let name = render_with(&cfg.sync.course_list_template, Some(c), &[]);
                        self.course_lists.insert(c.id, rec.ensure_list(&name)?);
                    }
                }
            }
        }

        log::info!("读取 To Do 任务");
        rec.load_tasks(cfg.sync.remove_duplicates)?;

        let fetcher = Fetcher(self.canvas);
        if let Some(list) = notification_list {
            log::info!("处理全局通知");
            match self.canvas.notifications() {
                Ok(ns) => {
                    for n in &ns {
                        let item = self.notification_item(n);
                        self.sync_one(rec, &list, item, &fetcher);
                    }
                }
                Err(e) => self.fail(rec, "读取全局通知", &e),
            }
        }

        for (i, course) in courses.iter().enumerate() {
            self.courses += 1;
            let name = if cfg.sync.verbose {
                course.name.clone()
            } else {
                (i + 1).to_string()
            };
            log::info!("处理课程 {name}");
            self.sync_course(rec, course, &fetcher);
        }
        Ok(())
    }

    fn list_for(&self, kind: Kind, course: &Course) -> Option<String> {
        match self.cfg.sync.list_name_mode {
            ListNameMode::Category => self.category_lists.get(&kind).cloned(),
            ListNameMode::Course => self.course_lists.get(&course.id).cloned(),
        }
    }

    fn fail(&mut self, rec: &mut Reconciler<'_>, what: &str, e: &anyhow::Error) {
        log::error!("{what}失败：{e:#}");
        self.errors += 1;
        rec.stats.failed += 1;
    }

    fn sync_one(
        &mut self,
        rec: &mut Reconciler<'_>,
        list: &str,
        item: SyncItem<'a>,
        fetcher: &Fetcher<'_>,
    ) {
        let label = item.label.clone();
        log::debug!("处理 {label}");
        if let Err(e) = rec.sync_item(list, item, fetcher) {
            self.fail(rec, &format!("同步 {label} "), &e);
        }
    }

    fn label(
        &self,
        rec: &Reconciler<'_>,
        kind: &str,
        title: &str,
        course: Option<&Course>,
    ) -> String {
        if self.cfg.sync.verbose {
            match course {
                Some(c) => format!("{} {kind} {title}", c.name),
                None => format!("{kind} {title}"),
            }
        } else {
            format!("{kind} #{}", rec.stats.items + 1)
        }
    }

    fn sync_course(&mut self, rec: &mut Reconciler<'_>, course: &Course, fetcher: &Fetcher<'_>) {
        let cfg = self.cfg;
        if cfg.assignment.enabled || cfg.quiz.enabled {
            match self.canvas.assignments(course.id) {
                Ok(list) => {
                    for a in &list {
                        let kind = if a.is_quiz_assignment {
                            Kind::Quiz
                        } else {
                            Kind::Assignment
                        };
                        let enabled = match kind {
                            Kind::Quiz => cfg.quiz.enabled,
                            _ => cfg.assignment.enabled,
                        };
                        if !enabled || self.too_old(a.due_at) {
                            continue;
                        }
                        let Some(target) = self.list_for(kind, course) else {
                            continue;
                        };
                        let item = self.assignment_item(rec, course, a, kind);
                        self.sync_one(rec, &target, item, fetcher);
                    }
                }
                Err(e) => self.fail(rec, &format!("读取课程 {} 的作业", course.id), &e),
            }
        }
        for (kind, topic_cfg) in [
            (Kind::Announcement, &cfg.announcement),
            (Kind::Discussion, &cfg.discussion),
        ] {
            if !topic_cfg.enabled {
                continue;
            }
            let res = match kind {
                Kind::Announcement => self.canvas.announcements(course.id),
                _ => self.canvas.discussions(course.id),
            };
            match res {
                Ok(topics) => {
                    for t in &topics {
                        if self.too_old(t.posted_at) {
                            continue;
                        }
                        let Some(target) = self.list_for(kind, course) else {
                            continue;
                        };
                        let item = self.topic_item(rec, course, t, kind, topic_cfg);
                        self.sync_one(rec, &target, item, fetcher);
                    }
                }
                Err(e) => self.fail(
                    rec,
                    &format!("读取课程 {} 的{}", course.id, kind.label()),
                    &e,
                ),
            }
        }
    }

    fn assignment_item(
        &self,
        rec: &Reconciler<'_>,
        course: &Course,
        a: &Assignment,
        kind: Kind,
    ) -> SyncItem<'a> {
        let cfg = self.cfg;
        let (template, policy, due_mode, fallback, remind_mode, before, comments) = match kind {
            Kind::Quiz => {
                let q = &cfg.quiz;
                (
                    &q.title_template,
                    q.policy(),
                    q.due_date_mode,
                    q.due_date_mode_fallback,
                    q.remind_mode,
                    q.remind_before.0,
                    Rule::new(false, false),
                )
            }
            _ => {
                let c = &cfg.assignment;
                (
                    &c.title_template,
                    c.policy(),
                    c.due_date_mode,
                    c.due_date_mode_fallback,
                    c.remind_mode,
                    c.remind_before.0,
                    Rule::new(c.create_comments, c.update_comments),
                )
            }
        };
        let title = render_with(
            template,
            Some(course),
            &[("assignment.title", &a.name), ("quiz.title", &a.name)],
        );

        let canvas = self.canvas;
        let tz = self.tz.clone();
        let (course_id, assignment_id, points) = (course.id, a.id, a.points_possible);
        let checklist: Option<super::ChecklistLoader<'a>> =
            match (kind, a.has_submitted_submissions, a.quiz_id) {
                (_, false, _) => None,
                (Kind::Quiz, true, Some(quiz_id)) => Some(Box::new(move |_| {
                    Ok(canvas
                        .quiz_submissions(course_id, quiz_id)?
                        .iter()
                        .map(|q| quiz_entry(q, &tz))
                        .collect())
                })),
                (Kind::Quiz, true, None) => None,
                _ => Some(Box::new(move |is_new| {
                    let s = canvas
                        .submission(course_id, assignment_id)?
                        .ok_or_else(|| anyhow::anyhow!("获取提交信息失败"))?;
                    Ok(submission_entries(
                        &s,
                        points,
                        comments.applies(is_new),
                        &tz,
                    ))
                })),
            };

        SyncItem {
            key: a.html_url.clone(),
            link: Link {
                web_url: Some(a.html_url.clone()),
                display_name: Some(a.html_url.clone()),
                application_name: Some("Canvas".into()),
            },
            label: self.label(rec, kind.label(), &a.name, Some(course)),
            title,
            body: html_to_text(a.description.as_deref().unwrap_or("")),
            due: pick_due(due_mode, fallback, a).map(|t| self.date(t)),
            reminder: pick_remind(remind_mode, before, a),
            checklist,
            attachments: if policy.attachments {
                extract_attachments(a.description.as_deref().unwrap_or(""))
            } else {
                Vec::new()
            },
            policy,
        }
    }

    fn topic_item(
        &self,
        rec: &Reconciler<'_>,
        course: &Course,
        t: &Topic,
        kind: Kind,
        cfg: &TopicConfig,
    ) -> SyncItem<'a> {
        let author = t
            .author
            .as_ref()
            .and_then(|a| a.display_name.as_deref())
            .unwrap_or("");
        let title = render_with(
            &cfg.title_template,
            Some(course),
            &[
                ("announcement.title", &t.title),
                ("anouncement.title", &t.title),
                ("announcement.author", author),
                ("anouncement.author", author),
                ("discussion.title", &t.title),
            ],
        );
        let message = t.message.as_deref().unwrap_or("");
        let content = format!("{}：\n{message}", t.user_name.as_deref().unwrap_or(""));
        let policy = cfg.policy();
        let mut attachments = Vec::new();
        if policy.attachments {
            attachments.extend(t.attachments.iter().filter_map(|f| {
                Some(AttachmentRef {
                    name: f.display_name.clone().or_else(|| f.filename.clone())?,
                    url: f.url.clone()?,
                })
            }));
            attachments.extend(extract_attachments(message));
        }
        SyncItem {
            key: t.html_url.clone(),
            link: Link {
                web_url: Some(t.html_url.clone()),
                display_name: Some(t.html_url.clone()),
                application_name: Some("Canvas".into()),
            },
            label: self.label(rec, kind.label(), &t.title, Some(course)),
            title,
            body: html_to_text(&content),
            due: t.posted_at.map(|p| self.date(p)),
            reminder: self.after_now(cfg.remind_after.0),
            checklist: None,
            attachments,
            policy,
        }
    }

    fn notification_item(&self, n: &Notification) -> SyncItem<'a> {
        let cfg = &self.cfg.notification;
        let id = n.id.to_string();
        let due = match cfg.due_date_mode {
            NotificationDueMode::StartAt => n.start_at,
            NotificationDueMode::EndAt => n.end_at,
        };
        SyncItem {
            key: format!("id:{id}"),
            link: Link {
                web_url: None,
                display_name: Some(id),
                application_name: Some(n.icon.clone().unwrap_or_else(|| "Canvas".into())),
            },
            label: if self.cfg.sync.verbose {
                format!("全局通知 {}", n.subject)
            } else {
                format!("全局通知 #{}", n.id)
            },
            title: render(&cfg.title_template, &[("notification.title", &n.subject)]),
            body: html_to_text(n.message.as_deref().unwrap_or("")),
            due: due.map(|t| self.date(t)),
            reminder: self.after_now(cfg.remind_after.0),
            checklist: None,
            attachments: Vec::new(),
            policy: cfg.policy(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::models::SubmissionComment;

    fn assignment(due: Option<&str>, lock: Option<&str>) -> Assignment {
        serde_json::from_value(serde_json::json!({
            "id": 1, "name": "HW1", "html_url": "https://c/courses/1/assignments/1",
            "due_at": due, "lock_at": lock,
        }))
        .unwrap()
    }

    #[test]
    fn due_and_remind_modes() {
        let a = assignment(None, Some("2022-10-20T15:59:59Z"));
        let lock: Timestamp = "2022-10-20T15:59:59Z".parse().unwrap();
        assert_eq!(pick_due(DueDateMode::DueAt, true, &a), Some(lock));
        assert_eq!(pick_due(DueDateMode::DueAt, false, &a), None);
        assert_eq!(
            pick_remind(RemindMode::BeforeLockAt, SignedDuration::from_hours(12), &a),
            Some("2022-10-20T03:59:59Z".parse().unwrap())
        );
        assert_eq!(
            pick_remind(RemindMode::BeforeDueAt, SignedDuration::from_hours(12), &a),
            None
        );
    }

    #[test]
    fn submission_text() {
        let tz = TimeZone::get("Asia/Shanghai").unwrap();
        let s = Submission {
            submitted_at: Some("2022-10-19T12:00:00Z".parse().unwrap()),
            grade: None,
            graded_at: None,
            submission_comments: vec![SubmissionComment {
                author_name: "助教".into(),
                comment: "未写完".into(),
            }],
        };
        let v = submission_entries(&s, Some(10.0), true, &tz);
        assert_eq!(
            v[0],
            ChecklistEntry {
                text: "已提交（提交时间：2022-10-19 20:00:00）".into(),
                checked: true
            }
        );
        assert_eq!(
            v[1],
            ChecklistEntry {
                text: "未评分".into(),
                checked: false
            }
        );
        assert_eq!(
            v[2],
            ChecklistEntry {
                text: "助教 评论：未写完".into(),
                checked: true
            }
        );
        assert_eq!(submission_entries(&s, Some(10.0), false, &tz).len(), 2);
    }

    #[test]
    fn quiz_text() {
        let tz = TimeZone::get("Asia/Shanghai").unwrap();
        let q: QuizSubmission = serde_json::from_value(serde_json::json!({
            "attempt": 1, "workflow_state": "pending_review", "score": 8.5,
            "quiz_points_possible": 10.0, "finished_at": "2022-10-19T12:00:00Z"
        }))
        .unwrap();
        let e = quiz_entry(&q, &tz);
        assert_eq!(
            e.text,
            "尝试 1：8.5/10（提交时间：2022-10-19 20:00:00）（*有些问题尚未计分）"
        );
        assert!(!e.checked);
    }

    #[test]
    fn templates() {
        let c: Course = serde_json::from_value(serde_json::json!({
            "id": 7, "name": "大学物理", "course_code": "本-(2021-2022-2)-PHY1251-6-大学物理(A类）（1）",
            "original_name": null, "enrollments": [{"type": "student"}]
        }))
        .unwrap();
        assert_eq!(
            render_with(
                "{course.coursecodeshort}|{course.originalname} - {quiz.title}",
                Some(&c),
                &[("quiz.title", "Q1")]
            ),
            "PHY1251|大学物理 - Q1"
        );
    }
}
