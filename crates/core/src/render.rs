//! Markdown to HTML for clients that display memos in a web view. The output is safe to insert
//! into the page: raw HTML in the source is shown as text, links are limited to `http`, `https` and
//! `mailto`, and images only load from attachments (remote images become links, so opening a memo
//! never contacts a third party).
//!
//! The markup the client fills in or reacts to:
//! - `<img class="attachment" data-attachment="<id>" alt="…">`: no `src`; the client sets a `blob:` URL.
//! - `<input type="checkbox" data-task="<offset>">`: a task box; [`toggle_task`] flips it in the source.
//! - `<span class="tag" data-tag="<lowercased>">#Tag</span>`.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
use uuid::Uuid;

use crate::text::{ATTACHMENT_SCHEME, find_tags};

const SAFE_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

pub fn to_html(body: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut events: Vec<Event> = Vec::new();
    // Open links: whether each one was emitted (unsafe ones are dropped, keeping their text).
    let mut links: Vec<bool> = Vec::new();
    let mut image: Option<(CowStr, String)> = None;
    let (mut in_code, mut boundary) = (false, true);
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        if let Some((_, alt)) = &mut image {
            match event {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => {
                    let (url, alt) = image.take().expect("inside an image");
                    events.push(Event::InlineHtml(image_html(&url, &alt).into()));
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => image = Some((dest_url, String::new())),
            Event::Start(Tag::Link { dest_url, .. }) => {
                let safe = is_safe_url(&dest_url);
                if safe {
                    let href = escape(&dest_url);
                    events.push(Event::InlineHtml(
                        format!(r#"<a href="{href}" rel="noopener noreferrer nofollow" target="_blank">"#).into(),
                    ));
                }
                links.push(safe);
            }
            Event::End(TagEnd::Link) => {
                if links.pop() == Some(true) {
                    events.push(Event::InlineHtml("</a>".into()));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                in_code = true;
                events.push(event);
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                events.push(event);
            }
            Event::Start(Tag::HtmlBlock) => events.push(Event::Html(r#"<pre class="html">"#.into())),
            Event::End(TagEnd::HtmlBlock) => events.push(Event::Html("</pre>".into())),
            Event::Html(text) | Event::InlineHtml(text) => events.push(Event::Text(text)),
            // Notes are written line by line: a single newline is a line break.
            Event::SoftBreak => {
                events.push(Event::HardBreak);
                boundary = true;
            }
            Event::TaskListMarker(checked) => {
                let checked = if checked { " checked" } else { "" };
                events.push(Event::InlineHtml(
                    format!(r#"<input type="checkbox" data-task="{}"{checked}>"#, range.start).into(),
                ));
                boundary = true;
            }
            Event::Text(text) if !in_code && links.is_empty() => {
                push_text_with_tags(&mut events, &text, boundary);
                boundary = text.ends_with(|c: char| c.is_whitespace() || c == '(');
            }
            Event::Text(_) | Event::Code(_) => {
                boundary = false;
                events.push(event);
            }
            Event::Start(Tag::Paragraph | Tag::Item | Tag::Heading { .. } | Tag::TableCell | Tag::BlockQuote(_)) => {
                boundary = true;
                events.push(event);
            }
            _ => events.push(event),
        }
    }
    let mut out = String::with_capacity(body.len() * 3 / 2);
    html::push_html(&mut out, events.into_iter());
    out
}

fn push_text_with_tags<'a>(events: &mut Vec<Event<'a>>, text: &str, boundary: bool) {
    let mut last = 0;
    for tag in find_tags(text, boundary) {
        if tag.start > last {
            events.push(Event::Text(text[last..tag.start].to_owned().into()));
        }
        let name = &text[tag.start + 1..tag.end];
        let html = format!(r#"<span class="tag" data-tag="{}">#{}</span>"#, escape(&name.to_lowercase()), escape(name));
        events.push(Event::InlineHtml(html.into()));
        last = tag.end;
    }
    if last < text.len() {
        events.push(Event::Text(text[last..].to_owned().into()));
    }
}

fn image_html(url: &str, alt: &str) -> String {
    let alt_attr = escape(alt);
    if let Some(id) = url.strip_prefix(ATTACHMENT_SCHEME).and_then(|id| Uuid::try_parse(id).ok()) {
        return format!(r#"<img class="attachment" data-attachment="{id}" alt="{alt_attr}">"#);
    }
    let label = if alt.is_empty() { escape(url) } else { alt_attr };
    if is_safe_url(url) {
        let href = escape(url);
        format!(
            r#"<a class="remote-image" href="{href}" rel="noopener noreferrer nofollow" target="_blank">{label}</a>"#
        )
    } else {
        label
    }
}

/// An absolute URL with an allowed scheme and nothing a browser might strip or reinterpret.
fn is_safe_url(url: &str) -> bool {
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    match url.split_once(':') {
        Some((scheme, _)) => SAFE_SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)),
        None => false,
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Flips the task box whose `data-task` offset is `offset`. `None` if there is no box there (the
/// body changed since it was rendered).
pub fn toggle_task(body: &str, offset: usize) -> Option<String> {
    let marker = body.get(offset..offset + 3)?;
    let replacement = match marker {
        "[ ]" => "[x]",
        "[x]" | "[X]" => "[ ]",
        _ => return None,
    };
    Some(format!("{}{replacement}{}", &body[..offset], &body[offset + 3..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_html_is_shown_as_text() {
        let html = to_html("<script>alert(1)</script>\n\nhi <b onclick=x>there</b>");
        assert!(!html.contains("<script") && !html.contains("<b "), "{html}");
        assert!(html.contains("&lt;script&gt;") && html.contains("&lt;b onclick=x&gt;"), "{html}");
    }

    #[test]
    fn only_safe_links_are_kept() {
        let html =
            to_html("[a](javascript:alert(1)) [b](https://example.org/?q=\"x\") [c](/relative) <mailto:me@x.org>");
        assert!(!html.contains("javascript"), "{html}");
        assert!(html.contains(r#"<a href="https://example.org/?q=&quot;x&quot;" rel="noopener noreferrer nofollow" target="_blank">b</a>"#), "{html}");
        assert!(!html.contains("/relative"), "{html}");
        assert!(html.contains(r#"href="mailto:me@x.org""#), "{html}");
    }

    #[test]
    fn images_load_only_from_attachments() {
        let id = "0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f";
        let html = to_html(&format!(
            "![a \"cat\"](attachment:{id}) ![tracker](https://evil.example/p.gif) ![x](data:image/png;base64,AA)"
        ));
        assert!(
            html.contains(&format!(r#"<img class="attachment" data-attachment="{id}" alt="a &quot;cat&quot;">"#)),
            "{html}"
        );
        assert!(html.contains(r#"<a class="remote-image" href="https://evil.example/p.gif""#), "{html}");
        assert!(!html.contains("<img src") && !html.contains("data:image"), "{html}");
    }

    #[test]
    fn tasks_tags_and_line_breaks() {
        let body = "- [ ] milk #shopping\n- [x] eggs\n\nline one\nline two `#notag`";
        let html = to_html(body);
        let first = body.find("[ ]").unwrap();
        let second = body.find("[x]").unwrap();
        assert!(html.contains(&format!(r#"<input type="checkbox" data-task="{first}">"#)), "{html}");
        assert!(html.contains(&format!(r#"<input type="checkbox" data-task="{second}" checked>"#)), "{html}");
        assert!(html.contains(r#"<span class="tag" data-tag="shopping">#shopping</span>"#), "{html}");
        assert!(html.contains("line one<br />"), "{html}");
        assert!(html.contains("<code>#notag</code>"), "{html}");
        let toggled = toggle_task(body, first).unwrap();
        assert!(toggled.starts_with("- [x] milk"));
        assert_eq!(toggle_task(&toggled, second).unwrap().matches("[ ]").count(), 1);
        assert_eq!(toggle_task(body, 0), None);
    }
}
