//! 文本处理：HTML 转换、模板替换、附件提取、时长解析等纯函数工具。

pub mod html;

use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use jiff::SignedDuration;
use regex_lite::Regex;

/// 简单的 `{key}` 占位符替换
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

static COURSE_CODE_SHORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(\d{4}-\d{4}-[123]\)-([a-zA-Z0-9]+?)-").unwrap());

/// 从形如 `本-(2021-2022-2)-PHY1251-6-大学物理` 的课程代码中提取 `PHY1251`，失败时原样返回
pub fn course_code_short(code: &str) -> String {
    COURSE_CODE_SHORT
        .captures(code)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| code.to_string())
}

/// 去掉列表名中的 Emoji（Microsoft To Do 允许在列表名前加 Emoji 图标）
pub fn clean_emoji(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            let u = c as u32;
            !(u > 0xFFFF
                || (0x2190..=0x21FF).contains(&u)
                || (0x2300..=0x23FF).contains(&u)
                || (0x2460..=0x27BF).contains(&u)
                || (0x2900..=0x297F).contains(&u)
                || (0x2B00..=0x2BFF).contains(&u)
                || (0xFE00..=0xFE0F).contains(&u)
                || matches!(
                    u,
                    0x00A9 | 0x00AE | 0x200D | 0x20E3 | 0x2122 | 0x3030 | 0x303D | 0x3297 | 0x3299
                ))
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// 统一换行并去掉首尾空白，用于比较备注内容
pub fn normalize_body(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_string()
}

/// 解析时长。支持：
/// - 简写：`5m`、`12h`、`1d`、`1d 12h 30m`、`90s`
/// - 原版 .NET TimeSpan 格式：`[d.]hh:mm:ss[.fffffff]`，如 `00:05:00.00`、`1.00:00:00`
pub fn parse_duration(input: &str) -> Result<SignedDuration> {
    let s = input.trim();
    if s.is_empty() {
        bail!("时长不能为空");
    }
    if s.contains(':') {
        return parse_timespan(s).with_context(|| format!("无法解析时长 `{input}`"));
    }
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut seen_unit = false;
    for c in s.chars().filter(|c| !c.is_whitespace()) {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: i64 = num
            .parse()
            .with_context(|| format!("无法解析时长 `{input}`"))?;
        let unit = match c {
            'w' => 7 * 86400,
            'd' => 86400,
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => bail!("无法解析时长 `{input}`：未知单位 `{c}`（可用 w/d/h/m/s）"),
        };
        total += n * unit;
        num.clear();
        seen_unit = true;
    }
    if !num.is_empty() || !seen_unit {
        bail!("无法解析时长 `{input}`：数字后缺少单位（可用 w/d/h/m/s）");
    }
    Ok(SignedDuration::from_secs(total))
}

fn parse_timespan(s: &str) -> Result<SignedDuration> {
    let (days, rest) = match s.split_once('.') {
        Some((d, r)) if !d.contains(':') => (d.parse::<i64>()?, r),
        _ => (0, s),
    };
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() != 3 {
        bail!("格式应为 [d.]hh:mm:ss");
    }
    let h: i64 = parts[0].parse()?;
    let m: i64 = parts[1].parse()?;
    let sec: f64 = parts[2].parse()?;
    Ok(SignedDuration::from_secs(days * 86400 + h * 3600 + m * 60)
        + SignedDuration::from_secs_f64(sec))
}

/// 一个待上传的附件引用
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRef {
    pub name: String,
    pub url: String,
}

static A_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<a\b([^>]*)>(.*?)</a\s*>").unwrap());
static IMG_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<img\b([^>]*)>").unwrap());
static ATTR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)([a-zA-Z_:][-a-zA-Z0-9_:.]*)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
});
static ANY_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());

fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    ATTR.captures_iter(attrs)
        .find(|c| c[1].eq_ignore_ascii_case(name))
        .and_then(|c| c.get(2).or_else(|| c.get(3)))
        .map(|m| m.as_str())
}

fn decode(s: &str) -> String {
    let s = html_escape::decode_html_entities(s);
    percent_encoding::percent_decode_str(&s)
        .decode_utf8_lossy()
        .trim()
        .to_string()
}

/// 从 Canvas 富文本中提取文件链接（`instructure_file_link`）和图片
pub fn extract_attachments(html: &str) -> Vec<AttachmentRef> {
    let mut out = Vec::new();
    for c in A_TAG.captures_iter(html) {
        let attrs = &c[1];
        if !attrs.contains("instructure_file_link") {
            continue;
        }
        let Some(href) = attr(attrs, "href") else {
            continue;
        };
        let name = attr(attrs, "title")
            .map(decode)
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| decode(&ANY_TAG.replace_all(&c[2], "")));
        if name.is_empty() {
            continue;
        }
        out.push(AttachmentRef {
            name,
            url: html_escape::decode_html_entities(href).into_owned(),
        });
    }
    for c in IMG_TAG.captures_iter(html) {
        let attrs = &c[1];
        let (Some(src), Some(alt)) = (attr(attrs, "src"), attr(attrs, "alt")) else {
            continue;
        };
        let name = decode(alt);
        if name.is_empty() || src.starts_with("data:") {
            continue;
        }
        out.push(AttachmentRef {
            name,
            url: html_escape::decode_html_entities(src).into_owned(),
        });
    }
    out
}

/// 数字格式化：整数不带小数点
pub fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5m").unwrap(), SignedDuration::from_mins(5));
        assert_eq!(
            parse_duration("1d 12h").unwrap(),
            SignedDuration::from_hours(36)
        );
        assert_eq!(
            parse_duration("00:05:00.00").unwrap(),
            SignedDuration::from_mins(5)
        );
        assert_eq!(
            parse_duration("1.00:00:00.00").unwrap(),
            SignedDuration::from_hours(24)
        );
        assert_eq!(
            parse_duration("12:00:00.00").unwrap(),
            SignedDuration::from_hours(12)
        );
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn course_code() {
        assert_eq!(
            course_code_short("本-(2021-2022-2)-PHY1251-6-大学物理(A类）（1）"),
            "PHY1251"
        );
        assert_eq!(course_code_short("CS101"), "CS101");
    }

    #[test]
    fn emoji() {
        assert_eq!(clean_emoji("📚 Canvas 作业"), "Canvas 作业");
        assert_eq!(clean_emoji("✏️Canvas"), "Canvas");
    }

    #[test]
    fn attachments() {
        let html = r#"<p><a class="instructure_file_link instructure_scribd_file" title="第1章.pdf" href="https://oc.sjtu.edu.cn/courses/1/files/2/download?wrap=1&amp;verifier=x">第1章.pdf</a>
<a href="/courses/1/files/3/download" class="instructure_file_link">讲义%20v2.pptx</a>
<a href="https://example.com">普通链接</a>
<img src="/courses/1/files/4/preview" alt="figure.png" /></p>"#;
        let got = extract_attachments(html);
        assert_eq!(
            got,
            vec![
                AttachmentRef {
                    name: "第1章.pdf".into(),
                    url: "https://oc.sjtu.edu.cn/courses/1/files/2/download?wrap=1&verifier=x"
                        .into()
                },
                AttachmentRef {
                    name: "讲义 v2.pptx".into(),
                    url: "/courses/1/files/3/download".into()
                },
                AttachmentRef {
                    name: "figure.png".into(),
                    url: "/courses/1/files/4/preview".into()
                },
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(fmt_num(10.0), "10");
        assert_eq!(fmt_num(9.5), "9.5");
        assert_eq!(fmt_num(1.0 / 3.0), "0.33");
    }
}
