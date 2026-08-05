use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy)]
pub struct MarkdownTheme {
    pub text: Color,
    pub accent: Color,
    pub muted: Color,
    pub code_background: Color,
}

pub fn render_markdown(source: &str, theme: MarkdownTheme) -> Vec<Line<'static>> {
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS,
    );
    let mut renderer = Renderer::new(theme);

    for event in parser {
        renderer.event(event);
    }

    renderer.finish()
}

struct ListState {
    next: Option<u64>,
}

struct Renderer {
    theme: MarkdownTheme,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    lists: Vec<ListState>,
    heading: bool,
    emphasis_depth: usize,
    strong_depth: usize,
    strikethrough_depth: usize,
    link_depth: usize,
    links: Vec<String>,
    code_block: bool,
    quote_depth: usize,
}

impl Renderer {
    fn new(theme: MarkdownTheme) -> Self {
        Self {
            theme,
            lines: Vec::new(),
            current: Vec::new(),
            lists: Vec::new(),
            heading: false,
            emphasis_depth: 0,
            strong_depth: 0,
            strikethrough_depth: 0,
            link_depth: 0,
            links: Vec::new(),
            code_block: false,
            quote_depth: 0,
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => {
                let style = self.inline_style().bg(self.theme.code_background);
                self.push_text_with_style(&code, style);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                self.push_text_with_style(&html, Style::default().fg(self.theme.muted));
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.finish_line(),
            Event::Rule => {
                self.finish_line();
                self.lines.push(Line::from(Span::styled(
                    "────────────────────────",
                    Style::default().fg(self.theme.muted),
                )));
            }
            Event::TaskListMarker(checked) => {
                self.push_span(
                    if checked { "☑ " } else { "☐ " },
                    Style::default().fg(if checked {
                        self.theme.accent
                    } else {
                        self.theme.muted
                    }),
                );
            }
            Event::FootnoteReference(reference) => {
                self.push_span(
                    format!("[{reference}]"),
                    Style::default().fg(self.theme.accent),
                );
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => {
                self.finish_line();
                self.heading = true;
            }
            Tag::BlockQuote(_) => {
                self.finish_line();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.finish_line();
                self.code_block = true;
                let language = match kind {
                    CodeBlockKind::Indented => String::new(),
                    CodeBlockKind::Fenced(language) => truncate_width(language.as_ref(), 32),
                };
                let label = if language.is_empty() {
                    "┌─ code".to_owned()
                } else {
                    format!("┌─ {language}")
                };
                self.lines.push(Line::from(Span::styled(
                    label,
                    Style::default().fg(self.theme.muted),
                )));
            }
            Tag::List(start) => {
                self.finish_line();
                self.lists.push(ListState { next: start });
            }
            Tag::Item => {
                self.finish_line();
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                let marker = self
                    .lists
                    .last_mut()
                    .map(|list| match &mut list.next {
                        Some(next) => {
                            let marker = format!("{next}. ");
                            *next += 1;
                            marker
                        }
                        None => "• ".to_owned(),
                    })
                    .unwrap_or_default();
                self.push_span(
                    format!("{indent}{marker}"),
                    Style::default().fg(self.theme.accent),
                );
            }
            Tag::Emphasis => self.emphasis_depth += 1,
            Tag::Strong => self.strong_depth += 1,
            Tag::Strikethrough => self.strikethrough_depth += 1,
            Tag::Link { dest_url, .. } => {
                self.link_depth += 1;
                self.links.push(dest_url.into_string());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.finish_line();
                self.blank_line();
            }
            TagEnd::Heading(_) => {
                self.finish_line();
                self.heading = false;
                self.blank_line();
            }
            TagEnd::BlockQuote(_) => {
                self.finish_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.blank_line();
            }
            TagEnd::CodeBlock => {
                self.finish_line();
                self.lines.push(Line::from(Span::styled(
                    "└─",
                    Style::default().fg(self.theme.muted),
                )));
                self.blank_line();
                self.code_block = false;
            }
            TagEnd::List(_) => {
                self.finish_line();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Item => self.finish_line(),
            TagEnd::Emphasis => self.emphasis_depth = self.emphasis_depth.saturating_sub(1),
            TagEnd::Strong => self.strong_depth = self.strong_depth.saturating_sub(1),
            TagEnd::Strikethrough => {
                self.strikethrough_depth = self.strikethrough_depth.saturating_sub(1);
            }
            TagEnd::Link => {
                self.link_depth = self.link_depth.saturating_sub(1);
                if let Some(destination) = self.links.pop() {
                    self.push_span(
                        format!(" ({destination})"),
                        Style::default().fg(self.theme.muted),
                    );
                }
            }
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        self.push_text_with_style(text, self.inline_style());
    }

    fn push_text_with_style(&mut self, text: &str, style: Style) {
        let mut segments = text.split('\n').peekable();
        while let Some(segment) = segments.next() {
            if !segment.is_empty() {
                self.push_span(segment.to_owned(), style);
            }
            if segments.peek().is_some() {
                self.finish_line();
            }
        }
    }

    fn push_span(&mut self, content: impl Into<String>, style: Style) {
        if self.current.is_empty() && self.quote_depth > 0 {
            self.current.push(Span::styled(
                format!("{} ", "│".repeat(self.quote_depth)),
                Style::default().fg(self.theme.muted),
            ));
        }
        self.current.push(Span::styled(content.into(), style));
    }

    fn inline_style(&self) -> Style {
        let mut style = Style::default().fg(if self.link_depth > 0 {
            self.theme.accent
        } else {
            self.theme.text
        });
        if self.heading || self.strong_depth > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.emphasis_depth > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strikethrough_depth > 0 {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.code_block {
            style = style.bg(self.theme.code_background);
        }
        style
    }

    fn finish_line(&mut self) {
        if !self.current.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.current)));
        }
    }

    fn blank_line(&mut self) {
        if self
            .lines
            .last()
            .is_some_and(|line| !line.spans.is_empty())
        {
            self.lines.push(Line::default());
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.current.is_empty() {
            self.finish_line();
        }
        while self
            .lines
            .last()
            .is_some_and(|line| line.spans.is_empty())
        {
            self.lines.pop();
        }
        self.lines
    }
}

fn truncate_width(value: &str, max_width: usize) -> String {
    let mut width = 0;
    value
        .chars()
        .take_while(|character| {
            let next = width + character.width().unwrap_or(0);
            if next > max_width {
                false
            } else {
                width = next;
                true
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> MarkdownTheme {
        MarkdownTheme {
            text: Color::White,
            accent: Color::Cyan,
            muted: Color::DarkGray,
            code_background: Color::Black,
        }
    }

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn renders_common_markdown_blocks() {
        let lines = render_markdown(
            "# Result\n\nA **bold** [link](https://example.com).\n\n- one\n- two",
            theme(),
        );
        let text = plain(&lines);

        assert!(text.contains(&"Result".to_owned()));
        assert!(text.contains(&"A bold link (https://example.com).".to_owned()));
        assert!(text.contains(&"• one".to_owned()));
        assert!(text.contains(&"• two".to_owned()));
    }

    #[test]
    fn renders_block_quotes_with_a_gutter() {
        let lines = render_markdown("> quoted text", theme());
        assert_eq!(plain(&lines), vec!["│ quoted text"]);
    }

    #[test]
    fn preserves_fenced_code_lines_and_language() {
        let lines = render_markdown("~~~rust\nfn main() {}\n~~~", theme());
        let text = plain(&lines);

        assert_eq!(text, vec!["┌─ rust", "fn main() {}", "└─"]);
    }

    #[test]
    fn handles_unicode_fence_labels_without_splitting_characters() {
        assert_eq!(truncate_width("rust-🦀-语言", 7), "rust-🦀");
    }
}
