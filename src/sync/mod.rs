//! 通用同步引擎：把来源（如 Canvas）产生的 [`SyncItem`] 与待办服务中的任务对齐。
//!
//! - 以关联链接作为唯一标识去重（与原版兼容，旧版本创建的任务会被识别）
//! - Create*/Update* 策略分开控制：新建时写入哪些字段、已存在时覆盖哪些字段
//! - 步骤（checklist）按位置对齐，附件按文件名去重

pub mod canvas;

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use jiff::civil::Date;
use jiff::{Timestamp, Unit};

use crate::text::{AttachmentRef, clean_emoji, normalize_body};
use crate::todo::{Link, Task, TaskFields, TaskList, TodoBackend};

/// 单个字段在“新建”与“已存在”两种情况下是否写入
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub on_create: bool,
    pub on_update: bool,
}

impl Rule {
    pub const fn new(on_create: bool, on_update: bool) -> Self {
        Self {
            on_create,
            on_update,
        }
    }

    pub fn applies(self, is_new: bool) -> bool {
        if is_new {
            self.on_create
        } else {
            self.on_update
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// 新建时总会写入标题
    pub update_title: bool,
    pub content: Rule,
    pub due: Rule,
    pub reminder: Rule,
    pub importance: Rule,
    /// importance 规则生效时设置的值：true 为“重要”
    pub important: bool,
    pub checklist: Rule,
    /// 检查并补传缺失的附件
    pub attachments: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistEntry {
    pub text: String,
    pub checked: bool,
}

/// 延迟加载步骤，仅在需要时才请求来源（参数为该条目是否为新建）
pub type ChecklistLoader<'a> = Box<dyn FnOnce(bool) -> Result<Vec<ChecklistEntry>> + 'a>;

pub struct SyncItem<'a> {
    /// 去重键，须与 `link.key()` 一致
    pub key: String,
    pub link: Link,
    /// 日志中显示的名称
    pub label: String,
    pub title: String,
    pub body: String,
    pub due: Option<Date>,
    pub reminder: Option<Timestamp>,
    pub checklist: Option<ChecklistLoader<'a>>,
    pub attachments: Vec<AttachmentRef>,
    pub policy: Policy,
}

/// 下载附件内容；返回 None 表示文件过大等原因跳过
pub trait AttachmentFetcher {
    fn fetch(&self, url: &str) -> Result<Option<Vec<u8>>>;
}

#[derive(Debug, Default)]
pub struct Stats {
    pub items: usize,
    pub created: usize,
    pub updated: usize,
    pub failed: usize,
    pub duplicates: usize,
}

struct Indexed {
    list_id: String,
    task: Task,
}

pub struct Reconciler<'b> {
    backend: &'b dyn TodoBackend,
    dry_run: bool,
    lists: Vec<TaskList>,
    resolved: HashMap<String, String>,
    index: HashMap<String, Indexed>,
    pub stats: Stats,
}

/// Microsoft To Do 单个附件上限
pub const ATTACHMENT_LIMIT: u64 = 25 * 1024 * 1024;

impl<'b> Reconciler<'b> {
    pub fn new(backend: &'b dyn TodoBackend, dry_run: bool) -> Result<Self> {
        Ok(Self {
            backend,
            dry_run,
            lists: backend.lists()?,
            resolved: HashMap::new(),
            index: HashMap::new(),
            stats: Stats::default(),
        })
    }

    /// 按名称查找列表（忽略名称中的 Emoji），不存在则创建；返回列表 ID
    pub fn ensure_list(&mut self, name: &str) -> Result<String> {
        if let Some(id) = self.resolved.get(name) {
            return Ok(id.clone());
        }
        let wanted = clean_emoji(name);
        let id = match self.lists.iter().find(|l| clean_emoji(&l.name) == wanted) {
            Some(l) => {
                log::info!("找到 To Do 列表：{}", l.name);
                l.id.clone()
            }
            None if self.dry_run => {
                log::info!("[dry-run] 将创建 To Do 列表：{name}");
                format!("dry-run:{name}")
            }
            None => {
                let l = self.backend.create_list(name)?;
                log::info!("已创建 To Do 列表：{}", l.name);
                let id = l.id.clone();
                self.lists.push(l);
                id
            }
        };
        self.resolved.insert(name.to_string(), id.clone());
        Ok(id)
    }

    /// 读取各列表中的任务并按关联链接建立索引
    pub fn load_tasks(&mut self, remove_duplicates: bool) -> Result<()> {
        let ids: Vec<String> = {
            let mut seen = HashSet::new();
            self.resolved
                .values()
                .filter(|id| !id.starts_with("dry-run:") && seen.insert((*id).clone()))
                .cloned()
                .collect()
        };
        for list_id in ids {
            for task in self.backend.tasks(&list_id)? {
                let Some(key) = task.links.first().and_then(Link::key) else {
                    continue;
                };
                if self.index.contains_key(&key) {
                    self.stats.duplicates += 1;
                    let title = task.title.as_deref().unwrap_or("");
                    if remove_duplicates && !self.dry_run {
                        log::warn!("删除重复任务：{title}（{key}）");
                        self.backend.delete_task(&list_id, &task.id)?;
                    } else {
                        log::warn!("发现重复任务：{title}（{key}），保留先找到的一个");
                    }
                    continue;
                }
                self.index.insert(
                    key,
                    Indexed {
                        list_id: list_id.clone(),
                        task,
                    },
                );
            }
        }
        log::info!("已读取 {} 个同步过的任务", self.index.len());
        Ok(())
    }

    /// 同步单个条目到 `list_id`（若任务已存在于其他受管列表，则在原列表中更新）。
    /// 返回是否产生了修改。
    pub fn sync_item(
        &mut self,
        list_id: &str,
        item: SyncItem<'_>,
        fetcher: &dyn AttachmentFetcher,
    ) -> Result<bool> {
        self.stats.items += 1;
        let existing = self.index.get(&item.key);
        let is_new = existing.is_none();
        let fields = diff(&item, existing.map(|e| &e.task));
        let list_id = existing.map_or(list_id, |e| e.list_id.as_str()).to_string();
        let mut changed = false;

        let task_id = if is_new {
            changed = true;
            self.stats.created += 1;
            if self.dry_run {
                log::info!("[dry-run] 将创建：{}", item.title);
                None
            } else {
                let task = self.backend.create_task(&list_id, &fields, &item.link)?;
                log::info!("已创建：{}", item.label);
                let id = task.id.clone();
                self.index.insert(
                    item.key.clone(),
                    Indexed {
                        list_id: list_id.clone(),
                        task,
                    },
                );
                Some(id)
            }
        } else {
            let task_id = existing.map(|e| e.task.id.clone());
            if !fields.is_empty() {
                changed = true;
                log::info!(
                    "{}更新{}：{}",
                    if self.dry_run { "[dry-run] 将" } else { "已" },
                    fields.describe(),
                    item.label
                );
                if !self.dry_run {
                    let id = task_id.as_deref().unwrap();
                    self.backend.update_task(&list_id, id, &fields)?;
                    if let Some(e) = self.index.get_mut(&item.key) {
                        fields.apply_to(&mut e.task);
                    }
                }
            }
            task_id
        };

        if let (Some(loader), Some(task_id)) = (item.checklist, &task_id)
            && item.policy.checklist.applies(is_new)
        {
            let entries = loader(is_new)?;
            changed |= self.sync_checklist(&list_id, task_id, &entries)?;
        }

        if item.policy.attachments
            && !item.attachments.is_empty()
            && let Some(task_id) = &task_id
        {
            match self.sync_attachments(&list_id, task_id, &item.attachments, fetcher) {
                Ok(c) => changed |= c,
                // 与原版一致：附件失败不视为同步错误
                Err(e) => log::warn!("上传附件失败（{}）：{e:#}", item.label),
            }
        }

        if changed && !is_new {
            self.stats.updated += 1;
        }
        Ok(changed)
    }

    /// 步骤按位置一一对应：不足则添加，不同则更新
    fn sync_checklist(
        &self,
        list_id: &str,
        task_id: &str,
        entries: &[ChecklistEntry],
    ) -> Result<bool> {
        if entries.is_empty() {
            return Ok(false);
        }
        let existing = self.backend.checklist(list_id, task_id)?;
        let mut changed = false;
        for (i, e) in entries.iter().enumerate() {
            match existing.get(i) {
                None => {
                    changed = true;
                    if self.dry_run {
                        log::info!("[dry-run] 将添加步骤：{}", e.text);
                    } else {
                        self.backend
                            .add_checklist_item(list_id, task_id, &e.text, e.checked)?;
                    }
                }
                Some(old) => {
                    let text_changed = old.text != e.text;
                    if text_changed || old.checked != e.checked {
                        changed = true;
                        if self.dry_run {
                            log::info!("[dry-run] 将更新步骤：{}", e.text);
                        } else {
                            self.backend.update_checklist_item(
                                list_id,
                                task_id,
                                &old.id,
                                text_changed.then_some(e.text.as_str()),
                                e.checked,
                            )?;
                        }
                    }
                }
            }
        }
        Ok(changed)
    }

    fn sync_attachments(
        &self,
        list_id: &str,
        task_id: &str,
        files: &[AttachmentRef],
        fetcher: &dyn AttachmentFetcher,
    ) -> Result<bool> {
        let mut present: HashSet<String> = self
            .backend
            .attachment_names(list_id, task_id)?
            .into_iter()
            .collect();
        let mut changed = false;
        for f in files {
            if present.contains(&f.name) {
                continue;
            }
            let data = match fetcher.fetch(&f.url) {
                Ok(Some(d)) => d,
                Ok(None) => {
                    log::info!("附件超过 25 MB，跳过：{}", f.name);
                    present.insert(f.name.clone());
                    continue;
                }
                Err(e) => {
                    log::warn!("下载附件失败：{}：{e:#}", f.name);
                    continue;
                }
            };
            if self.dry_run {
                log::info!("[dry-run] 将上传附件：{}（{} 字节）", f.name, data.len());
            } else {
                self.backend
                    .upload_attachment(list_id, task_id, &f.name, &data)?;
                log::info!("已上传附件：{}", f.name);
            }
            present.insert(f.name.clone());
            changed = true;
        }
        Ok(changed)
    }
}

fn round(t: Timestamp) -> Timestamp {
    t.round(Unit::Second).unwrap_or(t)
}

/// 计算需要写入的字段（移植自原版 `UpdateCanvasItem`）
fn diff(item: &SyncItem<'_>, old: Option<&Task>) -> TaskFields {
    let p = &item.policy;
    let is_new = old.is_none();
    let mut f = TaskFields::default();

    if is_new || p.update_title {
        let changed = old
            .and_then(|o| o.title.as_deref())
            .is_none_or(|t| t.trim() != item.title.trim());
        if changed {
            f.title = Some(item.title.clone());
        }
    }

    if p.content.applies(is_new) {
        let changed = old
            .and_then(|o| o.body.as_deref())
            .is_none_or(|b| normalize_body(b) != normalize_body(&item.body));
        if changed {
            f.body = Some(item.body.clone());
        }
    }

    if p.due.applies(is_new) {
        match (item.due, old) {
            (Some(d), None) => f.due = Some(Some(d)),
            (Some(d), Some(o)) if o.due != Some(d) => f.due = Some(Some(d)),
            (None, Some(o)) if o.due.is_some() => f.due = Some(None),
            _ => {}
        }
    }

    if p.reminder.applies(is_new)
        && let Some(r) = item.reminder
    {
        let r = round(r);
        let changed = match old {
            None => true,
            Some(o) => !o.reminder_on || o.reminder.map(round) != Some(r),
        };
        if changed {
            f.reminder = Some(r);
        }
    }

    if p.importance.applies(is_new) && old.is_none_or(|o| o.important != p.important) {
        f.important = Some(p.important);
    }

    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use crate::todo::ChecklistItem;

    const ALL: Policy = Policy {
        update_title: true,
        content: Rule::new(true, true),
        due: Rule::new(true, true),
        reminder: Rule::new(true, true),
        importance: Rule::new(false, false),
        important: false,
        checklist: Rule::new(true, true),
        attachments: true,
    };

    #[derive(Default)]
    struct Mock {
        tasks: RefCell<Vec<(String, Task)>>,
        checklist: RefCell<HashMap<String, Vec<ChecklistItem>>>,
        attachments: RefCell<HashMap<String, Vec<String>>>,
        calls: RefCell<Vec<String>>,
    }

    impl TodoBackend for Mock {
        fn lists(&self) -> Result<Vec<TaskList>> {
            Ok(vec![TaskList {
                id: "L1".into(),
                name: "📚 Canvas".into(),
            }])
        }
        fn create_list(&self, name: &str) -> Result<TaskList> {
            self.calls.borrow_mut().push(format!("create_list {name}"));
            Ok(TaskList {
                id: format!("new-{name}"),
                name: name.into(),
            })
        }
        fn tasks(&self, list_id: &str) -> Result<Vec<Task>> {
            Ok(self
                .tasks
                .borrow()
                .iter()
                .filter(|(l, _)| l == list_id)
                .map(|(_, t)| t.clone())
                .collect())
        }
        fn create_task(&self, list_id: &str, fields: &TaskFields, link: &Link) -> Result<Task> {
            let mut t = Task {
                id: format!("T{}", self.tasks.borrow().len() + 1),
                title: None,
                body: None,
                due: None,
                reminder: None,
                reminder_on: false,
                important: false,
                links: vec![link.clone()],
            };
            fields.apply_to(&mut t);
            self.calls
                .borrow_mut()
                .push(format!("create {list_id} {}", t.title.clone().unwrap()));
            self.tasks.borrow_mut().push((list_id.into(), t.clone()));
            Ok(t)
        }
        fn update_task(&self, _: &str, id: &str, fields: &TaskFields) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("update {id} {}", fields.describe()));
            Ok(())
        }
        fn delete_task(&self, _: &str, id: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("delete {id}"));
            Ok(())
        }
        fn checklist(&self, _: &str, id: &str) -> Result<Vec<ChecklistItem>> {
            Ok(self.checklist.borrow().get(id).cloned().unwrap_or_default())
        }
        fn add_checklist_item(&self, _: &str, id: &str, text: &str, _: bool) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("add_step {id} {text}"));
            Ok(())
        }
        fn update_checklist_item(
            &self,
            _: &str,
            _: &str,
            item: &str,
            text: Option<&str>,
            checked: bool,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("update_step {item} {text:?} {checked}"));
            Ok(())
        }
        fn attachment_names(&self, _: &str, id: &str) -> Result<Vec<String>> {
            Ok(self
                .attachments
                .borrow()
                .get(id)
                .cloned()
                .unwrap_or_default())
        }
        fn upload_attachment(&self, _: &str, id: &str, name: &str, _: &[u8]) -> Result<()> {
            self.calls.borrow_mut().push(format!("upload {id} {name}"));
            Ok(())
        }
    }

    struct Fetch;
    impl AttachmentFetcher for Fetch {
        fn fetch(&self, _: &str) -> Result<Option<Vec<u8>>> {
            Ok(Some(vec![1, 2, 3]))
        }
    }

    fn link(url: &str) -> Link {
        Link {
            web_url: Some(url.into()),
            display_name: Some(url.into()),
            application_name: Some("Canvas".into()),
        }
    }

    fn item(url: &str, title: &str) -> SyncItem<'static> {
        SyncItem {
            key: url.into(),
            link: link(url),
            label: title.into(),
            title: title.into(),
            body: "正文".into(),
            due: Some(jiff::civil::date(2022, 10, 20)),
            reminder: Some("2022-10-19T15:59:59Z".parse().unwrap()),
            checklist: None,
            attachments: vec![],
            policy: ALL,
        }
    }

    fn existing(id: &str, url: &str, title: &str) -> Task {
        Task {
            id: id.into(),
            title: Some(title.into()),
            body: Some("正文\r\n".into()),
            due: Some(jiff::civil::date(2022, 10, 20)),
            reminder: Some("2022-10-19T15:59:59Z".parse().unwrap()),
            reminder_on: true,
            important: false,
            links: vec![link(url)],
        }
    }

    #[test]
    fn creates_new_and_skips_unchanged() {
        let mock = Mock::default();
        mock.tasks
            .borrow_mut()
            .push(("L1".into(), existing("T0", "https://c/a/1", "作业1")));
        let mut r = Reconciler::new(&mock, false).unwrap();
        let l = r.ensure_list("Canvas").unwrap();
        assert_eq!(l, "L1", "列表匹配应忽略 Emoji");
        r.load_tasks(false).unwrap();

        assert!(
            !r.sync_item(&l, item("https://c/a/1", "作业1"), &Fetch)
                .unwrap()
        );
        assert!(
            r.sync_item(&l, item("https://c/a/2", "作业2"), &Fetch)
                .unwrap()
        );
        // 同一次运行内再次遇到同一条目不会重复创建
        assert!(
            !r.sync_item(&l, item("https://c/a/2", "作业2"), &Fetch)
                .unwrap()
        );
        assert_eq!(*mock.calls.borrow(), vec!["create L1 作业2"]);
    }

    #[test]
    fn updates_changed_fields_only() {
        let mock = Mock::default();
        mock.tasks
            .borrow_mut()
            .push(("L1".into(), existing("T0", "https://c/a/1", "旧标题")));
        let mut r = Reconciler::new(&mock, false).unwrap();
        let l = r.ensure_list("Canvas").unwrap();
        r.load_tasks(false).unwrap();
        let mut it = item("https://c/a/1", "新标题");
        it.due = None;
        assert!(r.sync_item(&l, it, &Fetch).unwrap());
        assert_eq!(*mock.calls.borrow(), vec!["update T0 标题、截止日期"]);
    }

    #[test]
    fn respects_update_policy() {
        let mock = Mock::default();
        mock.tasks
            .borrow_mut()
            .push(("L1".into(), existing("T0", "https://c/a/1", "我改过的标题")));
        let mut r = Reconciler::new(&mock, false).unwrap();
        let l = r.ensure_list("Canvas").unwrap();
        r.load_tasks(false).unwrap();
        let mut it = item("https://c/a/1", "原标题");
        it.policy.update_title = false;
        assert!(!r.sync_item(&l, it, &Fetch).unwrap());
        assert!(mock.calls.borrow().is_empty());
    }

    #[test]
    fn duplicates_detected_and_removed() {
        let mock = Mock::default();
        mock.tasks
            .borrow_mut()
            .push(("L1".into(), existing("T0", "https://c/a/1", "x")));
        mock.tasks
            .borrow_mut()
            .push(("L1".into(), existing("T9", "https://c/a/1", "x")));
        let mut r = Reconciler::new(&mock, false).unwrap();
        r.ensure_list("Canvas").unwrap();
        r.load_tasks(true).unwrap();
        assert_eq!(r.stats.duplicates, 1);
        assert_eq!(*mock.calls.borrow(), vec!["delete T9"]);
    }

    #[test]
    fn checklist_and_attachments() {
        let mock = Mock::default();
        mock.tasks
            .borrow_mut()
            .push(("L1".into(), existing("T0", "https://c/a/1", "作业1")));
        mock.checklist.borrow_mut().insert(
            "T0".into(),
            vec![ChecklistItem {
                id: "C1".into(),
                text: "未提交".into(),
                checked: false,
            }],
        );
        mock.attachments
            .borrow_mut()
            .insert("T0".into(), vec!["a.pdf".into()]);
        let mut r = Reconciler::new(&mock, false).unwrap();
        let l = r.ensure_list("Canvas").unwrap();
        r.load_tasks(false).unwrap();
        let mut it = item("https://c/a/1", "作业1");
        it.checklist = Some(Box::new(|is_new| {
            assert!(!is_new);
            Ok(vec![
                ChecklistEntry {
                    text: "已提交".into(),
                    checked: true,
                },
                ChecklistEntry {
                    text: "未评分".into(),
                    checked: false,
                },
            ])
        }));
        it.attachments = vec![
            AttachmentRef {
                name: "a.pdf".into(),
                url: "u1".into(),
            },
            AttachmentRef {
                name: "b.pdf".into(),
                url: "u2".into(),
            },
        ];
        assert!(r.sync_item(&l, it, &Fetch).unwrap());
        assert_eq!(
            *mock.calls.borrow(),
            vec![
                "update_step C1 Some(\"已提交\") true",
                "add_step T0 未评分",
                "upload T0 b.pdf",
            ]
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let mock = Mock::default();
        let mut r = Reconciler::new(&mock, true).unwrap();
        let l = r.ensure_list("新列表").unwrap();
        r.load_tasks(false).unwrap();
        assert!(
            r.sync_item(&l, item("https://c/a/1", "作业1"), &Fetch)
                .unwrap()
        );
        assert!(mock.calls.borrow().is_empty());
    }
}
