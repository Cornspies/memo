//! Markdown as a tree of blocks, for clients that draw memos with native widgets (Android). Same
//! rules as [`to_html`](super::to_html): raw HTML is text, only safe links are kept, images come
//! only from attachments, single newlines are line breaks, and tags and task boxes are marked.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::Serialize;
use uuid::Uuid;

use super::{OPTIONS, is_safe_url};
use crate::text::{ATTACHMENT_SCHEME, find_tags};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Paragraph {
        content: Vec<Inline>,
    },
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Quote {
        blocks: Vec<Block>,
    },
    /// `start` is set for ordered lists.
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    /// Fenced and indented code, and raw HTML blocks (shown as text).
    Code {
        text: String,
    },
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListItem {
    pub task: Option<Task>,
    pub blocks: Vec<Block>,
}

/// A task box. `offset` goes to [`text::toggle_task`](crate::text::toggle_task).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Task {
    pub checked: bool,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
        #[serde(flatten)]
        style: Style,
    },
    /// `#Name` as written; `tag` is the lowercased name the tag filter uses.
    Tag {
        name: String,
        tag: String,
    },
    Break,
    Image {
        attachment: Uuid,
        alt: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Style {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub code: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

pub fn to_blocks(body: &str) -> Vec<Block> {
    let mut b = Builder { stack: vec![Frame::Blocks(Vec::new())], ..Builder::default() };
    for (event, range) in Parser::new_ext(body, OPTIONS).into_offset_iter() {
        if let Some((_, alt)) = &mut b.image {
            match event {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => {
                    let (url, alt) = b.image.take().expect("inside an image");
                    b.image(&url, alt);
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => b.start(tag),
            Event::End(tag) => b.end(tag),
            Event::Text(text) if b.in_code => b.code_text(&text),
            Event::Text(text) if b.links.is_empty() => {
                b.text_with_tags(&text);
                b.boundary = text.ends_with(|c: char| c.is_whitespace() || c == '(');
            }
            Event::Text(text) => {
                b.boundary = false;
                b.text(&text, b.style());
            }
            Event::Code(text) => {
                b.boundary = false;
                b.text(&text, Style { code: true, ..b.style() });
            }
            Event::Html(text) if b.in_code => b.code_text(&text),
            Event::Html(text) | Event::InlineHtml(text) => b.text(&text, b.style()),
            Event::SoftBreak | Event::HardBreak => {
                b.inline(Inline::Break);
                b.boundary = true;
            }
            Event::TaskListMarker(checked) => {
                if let Some(Frame::Item(item)) = b.stack.last_mut() {
                    item.task = Some(Task { checked, offset: range.start });
                }
                b.boundary = true;
            }
            Event::Rule => b.block(Block::Rule),
            _ => {}
        }
    }
    b.finish()
}

#[derive(Debug)]
enum Frame {
    /// The document, or a quote.
    Blocks(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    /// `inlines` holds the text of a tight list item until a block or the end flushes it.
    Item(ItemFrame),
    Inlines {
        heading: Option<u8>,
        content: Vec<Inline>,
    },
    Code(String),
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Row(Vec<Vec<Inline>>),
    Cell(Vec<Inline>),
}

#[derive(Debug, Default)]
struct ItemFrame {
    task: Option<Task>,
    blocks: Vec<Block>,
    inlines: Vec<Inline>,
}

#[derive(Debug, Default)]
struct Builder {
    stack: Vec<Frame>,
    bold: usize,
    italic: usize,
    strike: usize,
    /// Open links: the URL of each safe one; unsafe ones keep only their text.
    links: Vec<Option<String>>,
    image: Option<(String, String)>,
    in_code: bool,
    in_head: bool,
    boundary: bool,
}

impl Builder {
    fn style(&self) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code: false,
            link: self.links.iter().rev().find_map(|l| l.clone()),
        }
    }

    fn start(&mut self, tag: Tag) {
        if matches!(tag, Tag::Paragraph | Tag::Item | Tag::Heading { .. } | Tag::TableCell | Tag::BlockQuote(_)) {
            self.boundary = true;
        }
        match tag {
            Tag::Paragraph => self.stack.push(Frame::Inlines { heading: None, content: Vec::new() }),
            Tag::Heading { level, .. } => {
                self.stack.push(Frame::Inlines { heading: Some(heading_level(level)), content: Vec::new() })
            }
            Tag::BlockQuote(_) => self.stack.push(Frame::Blocks(Vec::new())),
            Tag::CodeBlock(_) | Tag::HtmlBlock => {
                self.in_code = true;
                self.stack.push(Frame::Code(String::new()));
            }
            Tag::List(start) => self.stack.push(Frame::List { start, items: Vec::new() }),
            Tag::Item => self.stack.push(Frame::Item(ItemFrame::default())),
            Tag::Table(_) => self.stack.push(Frame::Table { head: Vec::new(), rows: Vec::new() }),
            Tag::TableHead => {
                self.in_head = true;
                self.stack.push(Frame::Row(Vec::new()));
            }
            Tag::TableRow => self.stack.push(Frame::Row(Vec::new())),
            Tag::TableCell => self.stack.push(Frame::Cell(Vec::new())),
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => {
                let url = dest_url.into_string();
                self.links.push(is_safe_url(&url).then_some(url));
            }
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.into_string(), String::new())),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                if let Some(Frame::Inlines { heading, content }) = self.stack.pop() {
                    self.block(match heading {
                        Some(level) => Block::Heading { level, content },
                        None => Block::Paragraph { content },
                    });
                }
            }
            TagEnd::BlockQuote(_) => {
                if let Some(Frame::Blocks(blocks)) = self.stack.pop() {
                    self.block(Block::Quote { blocks });
                }
            }
            TagEnd::CodeBlock | TagEnd::HtmlBlock => {
                self.in_code = false;
                if let Some(Frame::Code(mut text)) = self.stack.pop() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    self.block(Block::Code { text });
                }
            }
            TagEnd::List(_) => {
                if let Some(Frame::List { start, items }) = self.stack.pop() {
                    self.block(Block::List { start, items });
                }
            }
            TagEnd::Item => {
                if let Some(Frame::Item(mut item)) = self.stack.pop() {
                    flush_item(&mut item);
                    if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                        items.push(ListItem { task: item.task, blocks: item.blocks });
                    }
                }
            }
            TagEnd::TableHead => {
                self.in_head = false;
                if let Some(Frame::Row(cells)) = self.stack.pop()
                    && let Some(Frame::Table { head, .. }) = self.stack.last_mut()
                {
                    *head = cells;
                }
            }
            TagEnd::TableRow => {
                if let Some(Frame::Row(cells)) = self.stack.pop()
                    && let Some(Frame::Table { rows, .. }) = self.stack.last_mut()
                {
                    rows.push(cells);
                }
            }
            TagEnd::TableCell => {
                if let Some(Frame::Cell(content)) = self.stack.pop()
                    && let Some(Frame::Row(cells)) = self.stack.last_mut()
                {
                    cells.push(content);
                }
            }
            TagEnd::Table => {
                if let Some(Frame::Table { head, rows }) = self.stack.pop() {
                    self.block(Block::Table { head, rows });
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            }
            _ => {}
        }
    }

    fn block(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(Frame::Blocks(blocks)) => blocks.push(block),
            Some(Frame::Item(item)) => {
                flush_item(item);
                item.blocks.push(block);
            }
            _ => {}
        }
    }

    fn inline(&mut self, inline: Inline) {
        let content = match self.stack.last_mut() {
            Some(Frame::Inlines { content, .. } | Frame::Cell(content)) => content,
            Some(Frame::Item(item)) => &mut item.inlines,
            _ => return,
        };
        if let (Some(Inline::Text { text, style }), Inline::Text { text: more, style: next }) =
            (content.last_mut(), &inline)
            && style == next
        {
            text.push_str(more);
            return;
        }
        content.push(inline);
    }

    fn text(&mut self, text: &str, style: Style) {
        if !text.is_empty() {
            self.inline(Inline::Text { text: text.into(), style });
        }
    }

    fn code_text(&mut self, text: &str) {
        if let Some(Frame::Code(code)) = self.stack.last_mut() {
            code.push_str(text);
        }
    }

    fn text_with_tags(&mut self, text: &str) {
        let style = self.style();
        let mut last = 0;
        for tag in find_tags(text, self.boundary) {
            self.text(&text[last..tag.start], style.clone());
            let name = &text[tag.start + 1..tag.end];
            self.inline(Inline::Tag { name: name.into(), tag: name.to_lowercase() });
            last = tag.end;
        }
        self.text(&text[last..], style);
    }

    fn image(&mut self, url: &str, alt: String) {
        if let Some(id) = url.strip_prefix(ATTACHMENT_SCHEME).and_then(|id| Uuid::try_parse(id).ok()) {
            return self.inline(Inline::Image { attachment: id, alt });
        }
        let label = if alt.is_empty() { url.to_owned() } else { alt };
        let link = is_safe_url(url).then(|| url.to_owned());
        self.text(&label, Style { link, ..self.style() });
    }

    fn finish(mut self) -> Vec<Block> {
        match self.stack.swap_remove(0) {
            Frame::Blocks(blocks) => blocks,
            _ => Vec::new(),
        }
    }
}

fn flush_item(item: &mut ItemFrame) {
    if !item.inlines.is_empty() {
        item.blocks.push(Block::Paragraph { content: std::mem::take(&mut item.inlines) });
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text { text: text.into(), style: Style::default() }
    }

    fn styled(text: &str, style: Style) -> Inline {
        Inline::Text { text: text.into(), style }
    }

    fn paragraph(content: Vec<Inline>) -> Block {
        Block::Paragraph { content }
    }

    #[test]
    fn raw_html_is_text() {
        assert_eq!(
            to_blocks("<script>alert(1)</script>\n\nhi <b onclick=x>there</b>"),
            vec![
                Block::Code { text: "<script>alert(1)</script>".into() },
                paragraph(vec![text("hi <b onclick=x>there</b>")]),
            ]
        );
    }

    #[test]
    fn only_safe_links_are_kept() {
        let link = |url: &str| Style { link: Some(url.into()), ..Style::default() };
        assert_eq!(
            to_blocks("[a](javascript:alert(1)) [**b**](https://example.org) [c](/relative) <mailto:me@x.org>"),
            vec![paragraph(vec![
                text("a "),
                styled("b", Style { bold: true, ..link("https://example.org") }),
                text(" c "),
                styled("mailto:me@x.org", link("mailto:me@x.org")),
            ])]
        );
    }

    #[test]
    fn images_come_only_from_attachments() {
        let id: Uuid = "0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f".parse().unwrap();
        let blocks = to_blocks(&format!(
            "![a `cat`](attachment:{id}) ![tracker](https://evil.example/p.gif) ![x](data:image/png;base64,AA)"
        ));
        assert_eq!(
            blocks,
            vec![paragraph(vec![
                Inline::Image { attachment: id, alt: "a cat".into() },
                text(" "),
                styled("tracker", Style { link: Some("https://evil.example/p.gif".into()), ..Style::default() }),
                text(" x"),
            ])]
        );
    }

    #[test]
    fn tasks_tags_breaks_and_lists() {
        let body = "- [ ] milk #Shopping\n- [x] eggs\n  1. nested\n\nline one\nline two `#notag`";
        let first = body.find("[ ]").unwrap();
        let second = body.find("[x]").unwrap();
        assert_eq!(
            to_blocks(body),
            vec![
                Block::List {
                    start: None,
                    items: vec![
                        ListItem {
                            task: Some(Task { checked: false, offset: first }),
                            blocks: vec![paragraph(vec![
                                text("milk "),
                                Inline::Tag { name: "Shopping".into(), tag: "shopping".into() },
                            ])],
                        },
                        ListItem {
                            task: Some(Task { checked: true, offset: second }),
                            blocks: vec![
                                paragraph(vec![text("eggs")]),
                                Block::List {
                                    start: Some(1),
                                    items: vec![ListItem { task: None, blocks: vec![paragraph(vec![text("nested")])] }],
                                },
                            ],
                        },
                    ],
                },
                paragraph(vec![
                    text("line one"),
                    Inline::Break,
                    text("line two "),
                    styled("#notag", Style { code: true, ..Style::default() }),
                ]),
            ]
        );
    }

    #[test]
    fn headings_quotes_code_and_tables() {
        let body = "# Title *x*\n\n> quoted\n> # inner\n\n```\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | ~~2~~ |\n\n---";
        assert_eq!(
            to_blocks(body),
            vec![
                Block::Heading {
                    level: 1,
                    content: vec![text("Title "), styled("x", Style { italic: true, ..Style::default() })],
                },
                Block::Quote {
                    blocks: vec![
                        paragraph(vec![text("quoted")]),
                        Block::Heading { level: 1, content: vec![text("inner")] },
                    ],
                },
                Block::Code { text: "fn main() {}".into() },
                Block::Table {
                    head: vec![vec![text("a")], vec![text("b")]],
                    rows: vec![vec![vec![text("1")], vec![styled("2", Style { strike: true, ..Style::default() })]]],
                },
                Block::Rule,
            ]
        );
    }

    #[test]
    fn json_shape() {
        let json = serde_json::to_string(&to_blocks("**hi** #t")).unwrap();
        assert_eq!(
            json,
            r#"[{"type":"paragraph","content":[{"type":"text","text":"hi","bold":true},{"type":"text","text":" "},{"type":"tag","name":"t","tag":"t"}]}]"#
        );
    }
}
