//! Plain-text views of a memo body: title, preview, tags and image references. Every client
//! uses these, so lists and tag filters look the same everywhere.

use std::collections::BTreeSet;

use uuid::Uuid;

use crate::payload::MemoPayload;

const TITLE_CHARS: usize = 100;
const PREVIEW_CHARS: usize = 200;
pub(crate) const ATTACHMENT_SCHEME: &str = "attachment:";

/// The first line with text, without Markdown markers.
pub fn title(body: &str) -> String {
    content_lines(body).next().map(|line| truncate(&line, TITLE_CHARS)).unwrap_or_default()
}

/// The text after the title, on one line.
pub fn preview(body: &str) -> String {
    let mut preview = String::new();
    for line in content_lines(body).skip(1) {
        if !preview.is_empty() {
            preview.push(' ');
        }
        preview.push_str(&line);
        if preview.chars().count() > PREVIEW_CHARS {
            break;
        }
    }
    truncate(&preview, PREVIEW_CHARS)
}

fn content_lines(body: &str) -> impl Iterator<Item = String> + '_ {
    let mut fence: Option<Fence> = None;
    body.lines().filter_map(move |line| {
        if let Some(open) = fence {
            if Fence::parse(line).is_some_and(|close| close.closes(open)) {
                fence = None;
            }
            return Some(line.trim().to_owned()).filter(|l| !l.is_empty());
        }
        if let Some(open) = Fence::parse(line) {
            fence = Some(open);
            return None;
        }
        let text = strip_markers(line);
        (!text.is_empty()).then_some(text)
    })
}

/// Drops block markers (headings, lists, quotes, task boxes), images and inline emphasis.
fn strip_markers(line: &str) -> String {
    let mut rest = line.trim();
    loop {
        let before = rest;
        rest = rest.trim_start_matches('>').trim_start();
        if let Some(r) = rest.strip_prefix(|c| c == '-' || c == '*' || c == '+').filter(|r| r.starts_with(' ')) {
            rest = r.trim_start();
        }
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0
            && let Some(r) = rest[digits..].strip_prefix(['.', ')']).filter(|r| r.starts_with(' '))
        {
            rest = r.trim_start();
        }
        for marker in ["[ ] ", "[x] ", "[X] "] {
            if let Some(r) = rest.strip_prefix(marker) {
                rest = r.trim_start();
            }
        }
        if rest == before {
            break;
        }
    }
    let hashes = rest.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && rest[hashes..].starts_with([' ', '\t']) || rest.len() == hashes && hashes > 0 {
        rest = rest[hashes..].trim();
    }
    let mut text = String::with_capacity(rest.len());
    let mut chars = rest.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // `![alt](url)` and `[text](url)` become `alt` and `text`.
        let bracket = if c == '!' && rest[i + 1..].starts_with('[') { Some(i + 1) } else { (c == '[').then_some(i) };
        if let Some(start) = bracket
            && let Some((label, len)) = bracketed(&rest[start..])
        {
            text.push_str(&strip_markers(label));
            while chars.peek().is_some_and(|&(j, _)| j < start + len) {
                chars.next();
            }
            continue;
        }
        if !matches!(c, '*' | '_' | '`' | '~') {
            text.push(c);
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `[label](…)` at the start of `s`: the label and the length of the whole construct.
fn bracketed(s: &str) -> Option<(&str, usize)> {
    let close = s.find("](")?;
    if s[1..close].contains(['[', ']']) {
        return None;
    }
    let end = s[close..].find(')')? + close;
    Some((&s[1..close], end + 1))
}

fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_owned(),
    }
}

/// Lowercased tags, sorted and without duplicates. A tag is `#` followed by letters, digits, `_`,
/// `-` or `/`, with at least one character that is not a digit. The `#` must start a line or follow
/// whitespace or `(`. Tags in code spans and fenced code blocks do not count.
pub fn tags(body: &str) -> Vec<String> {
    let mut tags = BTreeSet::new();
    let mut fence: Option<Fence> = None;
    for line in body.lines() {
        if let Some(open) = fence {
            if Fence::parse(line).is_some_and(|close| close.closes(open)) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = Fence::parse(line) {
            fence = Some(open);
            continue;
        }
        for segment in outside_code_spans(line) {
            for range in find_tags(segment, true) {
                tags.insert(segment[range.start + 1..range.end].to_lowercase());
            }
        }
    }
    tags.into_iter().collect()
}

/// Byte ranges of the tags in `text`, each including its `#`. `at_boundary` says whether the text
/// starts where a tag may start (the renderer passes text in pieces).
pub fn find_tags(text: &str, at_boundary: bool) -> Vec<std::ops::Range<usize>> {
    let mut found = Vec::new();
    let mut previous: Option<char> = None;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let boundary = match previous {
            None => at_boundary,
            Some(p) => p.is_whitespace() || p == '(',
        };
        previous = Some(c);
        if c != '#' || !boundary {
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while let Some(&(j, d)) = iter.peek() {
            if !is_tag_char(d) {
                break;
            }
            end = j + d.len_utf8();
            previous = Some(d);
            iter.next();
        }
        let name = text[start..end].trim_end_matches(['-', '/']);
        if !name.is_empty() && !name.chars().all(|d| d.is_ascii_digit()) {
            found.push(i..start + name.len());
        }
    }
    found
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// The parts of a line outside `` `code` `` spans.
fn outside_code_spans(line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let ticks = rest[open..].chars().take_while(|&c| c == '`').count();
        let after = &rest[open + ticks..];
        let delimiter = "`".repeat(ticks);
        match after.find(&delimiter) {
            Some(close) => {
                parts.push(&rest[..open]);
                rest = &after[close + ticks..];
            }
            None => break,
        }
    }
    parts.push(rest);
    parts
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Fence {
    char: char,
    len: usize,
}

impl Fence {
    /// A fence line: up to 3 spaces, then at least 3 backticks or tildes.
    fn parse(line: &str) -> Option<Self> {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            return None;
        }
        let rest = &line[indent..];
        let char = rest.chars().next().filter(|&c| c == '`' || c == '~')?;
        let len = rest.chars().take_while(|&c| c == char).count();
        (len >= 3).then_some(Self { char, len })
    }

    fn closes(self, open: Fence) -> bool {
        self.char == open.char && self.len >= open.len
    }
}

/// Attachment ids the body refers to as `attachment:<id>`.
pub fn referenced_attachments(body: &str) -> BTreeSet<Uuid> {
    body.match_indices(ATTACHMENT_SCHEME)
        .filter_map(|(i, _)| body.get(i + ATTACHMENT_SCHEME.len()..i + ATTACHMENT_SCHEME.len() + 36))
        .filter_map(|id| Uuid::try_parse(id).ok())
        .collect()
}

impl MemoPayload {
    /// Drops attachments the body no longer refers to. Clients call this when the editor closes,
    /// not on every keystroke, so cutting and pasting an image line does not delete the image.
    pub fn prune_attachments(&mut self) {
        let referenced = referenced_attachments(&self.body);
        self.attachments.retain(|a| referenced.contains(&a.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_and_preview_skip_markup() {
        let body = "\n# Groceries *today*\n\n- [ ] milk\n- [x] `eggs`\n> quoted ![photo](attachment:x) [**text**](https://x.org) [a]\n";
        assert_eq!(title(body), "Groceries today");
        assert_eq!(preview(body), "milk eggs quoted photo text [a]");
        assert_eq!(title("![](attachment:0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f)\nCaption"), "Caption");
        assert_eq!(title("#tag at start"), "#tag at start");
        assert_eq!(title("1. first\n2) second"), "first");
        assert_eq!(title(""), "");
        assert_eq!(title(&"x".repeat(150)).chars().count(), TITLE_CHARS + 1);
        assert_eq!(title("```\ncode line\n```"), "code line");
    }

    #[test]
    fn tags_follow_the_rules() {
        let body = "#Work and #home/garden, (#inbox) #2024 #v2 #todo-\n\
                    # Heading\nmail@#nope https://x.org/#frag `#code`\n```\n#fenced\n```\n#after";
        assert_eq!(tags(body), ["after", "home/garden", "inbox", "todo", "v2", "work"]);
        assert_eq!(tags("#日本 #Über"), ["über", "日本"]);
        assert_eq!(find_tags("x#y #z", false), vec![std::ops::Range { start: 4, end: 6 }]);
        assert_eq!(find_tags("#z", false), Vec::<std::ops::Range<usize>>::new());
    }

    #[test]
    fn prune_keeps_referenced_attachments() {
        let id: Uuid = "0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f".parse().unwrap();
        let other: Uuid = "0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e60".parse().unwrap();
        let body = format!("![a](attachment:{id}) and attachment:broken");
        assert_eq!(referenced_attachments(&body), BTreeSet::from([id]));
        let attachment = |id| crate::AttachmentRef {
            id,
            key: crate::AttachmentKey::generate(),
            mime: "image/png".into(),
            size: 1,
            width: 1,
            height: 1,
            thumb: None,
        };
        let mut payload = MemoPayload::new(body, 0);
        payload.attachments = vec![attachment(id), attachment(other)];
        payload.prune_attachments();
        assert_eq!(payload.attachments.iter().map(|a| a.id).collect::<Vec<_>>(), [id]);
    }
}
