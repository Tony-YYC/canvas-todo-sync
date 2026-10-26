//! HTML → 纯文本转换（移植自原项目 `HtmlHelper`，保持输出一致，
//! 以免旧版本创建的待办事项因备注格式差异被反复更新）。

use std::sync::LazyLock;

use regex_lite::Regex;

static ANCHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<a.*?href="(.+?)".+?>(.*?)</a>"#).unwrap());

/// 遇到这些标签时写入的分隔符
fn tag_separator(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "address" | "blockquote" | "div" | "dl" | "fieldset" | "form" | "h1" | "/h1" | "h2"
        | "/h2" | "h3" | "/h3" | "h4" | "/h4" | "h5" | "/h5" | "h6" | "/h6" | "p" | "/p"
        | "table" | "/table" | "ul" | "/ul" | "ol" | "/ol" | "/li" | "br" | "/tr" | "/pre" => "\n",
        "/td" => "\t",
        _ => return None,
    })
}

fn is_ignored(tag: &str) -> bool {
    matches!(tag, "script" | "noscript" | "style" | "object")
}

pub fn html_to_text(html: &str) -> String {
    // 把 <a href="x">y</a> 改写成 [y](x)
    let html = ANCHOR.replace_all(html, "[$2]($1)");
    let mut p = Parser {
        s: html.chars().collect(),
        pos: 0,
        out: TextBuilder::default(),
    };
    p.run();
    let text = p.out.finish();
    html_escape::decode_html_entities(&text).into_owned()
}

struct Parser {
    s: Vec<char>,
    pos: usize,
    out: TextBuilder,
}

impl Parser {
    fn eof(&self) -> bool {
        self.pos >= self.s.len()
    }

    fn peek(&self) -> char {
        self.s.get(self.pos).copied().unwrap_or('\0')
    }

    fn advance(&mut self) {
        self.pos = (self.pos + 1).min(self.s.len());
    }

    fn run(&mut self) {
        while !self.eof() {
            let c = self.peek();
            if c == '<' {
                let (tag, _) = self.parse_tag();
                match tag.as_str() {
                    "body" => self.out.clear(),
                    "/body" => self.pos = self.s.len(),
                    "pre" => {
                        self.out.set_preformatted(true);
                        self.eat_whitespace_to_next_line();
                    }
                    "/pre" => self.out.set_preformatted(false),
                    _ => {}
                }
                if let Some(sep) = tag_separator(&tag) {
                    self.out.write_str(sep);
                }
                if is_ignored(&tag) {
                    self.eat_inner_content(&tag);
                }
            } else if c.is_whitespace() {
                let w = if self.out.preformatted { c } else { ' ' };
                self.out.write(w);
                self.advance();
            } else {
                self.out.write(c);
                self.advance();
            }
        }
    }

    /// 读取一个完整标签，返回（小写标签名，是否自闭合）
    fn parse_tag(&mut self) -> (String, bool) {
        let mut self_closing = false;
        if self.peek() != '<' {
            return (String::new(), false);
        }
        self.advance();
        while self.peek().is_whitespace() {
            self.advance();
        }
        let start = self.pos;
        if self.peek() == '/' {
            self.advance();
        }
        while !self.eof()
            && !self.peek().is_whitespace()
            && self.peek() != '/'
            && self.peek() != '>'
        {
            self.advance();
        }
        let tag: String = self.s[start..self.pos]
            .iter()
            .collect::<String>()
            .to_lowercase();
        while !self.eof() && self.peek() != '>' {
            let c = self.peek();
            if c == '"' || c == '\'' {
                self.eat_quoted_value();
            } else {
                if c == '/' {
                    self_closing = true;
                }
                self.advance();
            }
        }
        self.advance();
        (tag, self_closing)
    }

    fn eat_inner_content(&mut self, tag: &str) {
        let end_tag = format!("/{tag}");
        while !self.eof() {
            if self.peek() == '<' {
                let (t, self_closing) = self.parse_tag();
                if t == end_tag {
                    return;
                }
                if !self_closing && !tag.starts_with('/') {
                    self.eat_inner_content(tag);
                }
            } else {
                self.advance();
            }
        }
    }

    fn eat_whitespace_to_next_line(&mut self) {
        while self.peek().is_whitespace() {
            let c = self.peek();
            self.advance();
            if c == '\n' {
                break;
            }
        }
    }

    fn eat_quoted_value(&mut self) {
        let q = self.peek();
        self.advance();
        match self.s[self.pos..]
            .iter()
            .position(|&c| c == q || c == '\r' || c == '\n')
        {
            Some(off) => {
                self.pos += off;
                self.advance();
            }
            None => self.pos = self.s.len(),
        }
    }
}

/// 合并多余空白、限制连续空行的文本缓冲
#[derive(Default)]
struct TextBuilder {
    text: String,
    line: String,
    empty_lines: usize,
    preformatted: bool,
}

impl TextBuilder {
    fn set_preformatted(&mut self, v: bool) {
        if v {
            if !self.line.is_empty() {
                self.flush_line();
            }
            self.empty_lines = 0;
        }
        self.preformatted = v;
    }

    fn clear(&mut self) {
        self.text.clear();
        self.line.clear();
        self.empty_lines = 0;
    }

    fn write_str(&mut self, s: &str) {
        for c in s.chars() {
            self.write(c);
        }
    }

    fn write(&mut self, c: char) {
        if self.preformatted {
            self.text.push(c);
        } else if c == '\r' {
        } else if c == '\n' {
            self.flush_line();
        } else if c.is_whitespace() {
            if !self.line.chars().last().is_some_and(char::is_whitespace) {
                self.line.push(' ');
            }
        } else {
            self.line.push(c);
        }
    }

    fn flush_line(&mut self) {
        let line = self.line.trim().to_string();
        if line.replace(' ', "").is_empty() {
            self.empty_lines += 1;
            if self.empty_lines < 2 && !self.text.is_empty() {
                self.text.push_str(&line);
                self.text.push('\n');
            }
        } else {
            self.empty_lines = 0;
            self.text.push_str(&line);
            self.text.push('\n');
        }
        self.line.clear();
    }

    fn finish(mut self) -> String {
        if !self.line.is_empty() {
            self.flush_line();
        }
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_and_links() {
        let html =
            r#"<p>第一段&nbsp;内容</p><p>见 <a href="https://x.y/z" target="_blank">链接</a></p>"#;
        assert_eq!(
            html_to_text(html),
            "第一段\u{a0}内容\n\n见 [链接](https://x.y/z)\n"
        );
    }

    #[test]
    fn script_ignored_and_whitespace_collapsed() {
        let html = "<div>a   b<script>var x = 1;</script></div>\n\n\n<div>c</div>";
        assert_eq!(html_to_text(html), "a b\nc\n");
    }
}
