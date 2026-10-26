//! Canvas REST API 数据模型（只保留同步需要的字段）。

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};

/// 宽松的时间解析：格式异常时视为空，避免单个字段导致整页解析失败
fn ts<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Timestamp>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    Ok(s.and_then(|s| s.parse().ok()))
}

/// 宽松的数字解析：兼容数字与字符串
fn num<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v: Option<serde_json::Value> = Option::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.parse().ok(),
        _ => None,
    })
}

fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
pub struct UserProfile {
    pub id: i64,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Course {
    pub id: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_default")]
    pub course_code: String,
    #[serde(default)]
    pub original_name: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub enrollments: Vec<Enrollment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Enrollment {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Assignment {
    pub id: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "ts")]
    pub due_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "ts")]
    pub lock_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "ts")]
    pub unlock_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "null_default")]
    pub html_url: String,
    #[serde(default, deserialize_with = "null_default")]
    pub is_quiz_assignment: bool,
    #[serde(default)]
    pub quiz_id: Option<i64>,
    #[serde(default, deserialize_with = "null_default")]
    pub has_submitted_submissions: bool,
    #[serde(default, deserialize_with = "num")]
    pub points_possible: Option<f64>,
}

/// 公告与讨论共用的 discussion_topic 模型
#[derive(Debug, Deserialize)]
pub struct Topic {
    #[serde(default, deserialize_with = "null_default")]
    pub title: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub html_url: String,
    #[serde(default, deserialize_with = "ts")]
    pub posted_at: Option<Timestamp>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub author: Option<Author>,
    #[serde(default, deserialize_with = "null_default")]
    pub attachments: Vec<FileAttachment>,
}

#[derive(Debug, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FileAttachment {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Submission {
    #[serde(default, deserialize_with = "ts")]
    pub submitted_at: Option<Timestamp>,
    #[serde(default)]
    pub grade: Option<String>,
    #[serde(default, deserialize_with = "ts")]
    pub graded_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "null_default")]
    pub submission_comments: Vec<SubmissionComment>,
}

#[derive(Debug, Deserialize)]
pub struct SubmissionComment {
    #[serde(default, deserialize_with = "null_default")]
    pub author_name: String,
    #[serde(default, deserialize_with = "null_default")]
    pub comment: String,
}

#[derive(Debug, Deserialize)]
pub struct QuizSubmissions {
    #[serde(default, deserialize_with = "null_default")]
    pub quiz_submissions: Vec<QuizSubmission>,
}

#[derive(Debug, Deserialize)]
pub struct QuizSubmission {
    #[serde(default)]
    pub attempt: Option<i64>,
    #[serde(default, deserialize_with = "null_default")]
    pub workflow_state: String,
    #[serde(default, deserialize_with = "num")]
    pub score: Option<f64>,
    #[serde(default, deserialize_with = "num")]
    pub quiz_points_possible: Option<f64>,
    #[serde(default, deserialize_with = "ts")]
    pub started_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "ts")]
    pub finished_at: Option<Timestamp>,
}

/// 全局通知（控制面板顶部横幅）
#[derive(Debug, Deserialize)]
pub struct Notification {
    pub id: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub subject: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, deserialize_with = "ts")]
    pub start_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "ts")]
    pub end_at: Option<Timestamp>,
    #[serde(default)]
    pub icon: Option<String>,
}
