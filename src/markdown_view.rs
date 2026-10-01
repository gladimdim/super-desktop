//! Markdown rendered as formatted text in a read-only `TextView`: headings,
//! emphasis, strikethrough, inline and fenced code, nested and task lists,
//! quotes, rules, tables and links. Nothing is fetched or executed: an image
//! shows its alt text and address, HTML stays literal text, and only a click on
//! a plain http(s) link opens it, in the default browser.
use gtk4::{gdk, gio, glib, prelude::*};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Text tags carrying a link are named this prefix plus the link's target.
const LINK: &str = "link ";

/// A read-only view showing `text` rendered.
pub fn view(text: &str) -> gtk4::TextView {
    let view = gtk4::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    view.set_left_margin(12);
    view.set_right_margin(12);
    view.set_top_margin(12);
    view.set_bottom_margin(12);
    view.add_css_class("markdown-view");
    render(&view, text);

    let click = gtk4::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    click.connect_released(|gesture, presses, x, y| {
        let Some(view) = gesture.widget().and_downcast::<gtk4::TextView>() else { return };
        // A drag that selected text is not a click on the link under it.
        if presses != 1 || view.buffer().has_selection() {
            return;
        }
        if let Some(url) = link_at(&view, x, y) {
            glib::MainContext::default().spawn_local(async move {
                if let Err(error) = gio::AppInfo::launch_default_for_uri_future(&url, None::<&gio::AppLaunchContext>).await {
                    eprintln!("Could not open Markdown link: {error}");
                }
            });
        }
    });
    view.add_controller(click);
    let motion = gtk4::EventControllerMotion::new();
    motion.connect_motion(|motion, x, y| {
        let Some(view) = motion.widget().and_downcast::<gtk4::TextView>() else { return };
        let cursor = if link_at(&view, x, y).is_some() { "pointer" } else { "text" };
        view.set_cursor_from_name(Some(cursor));
    });
    view.add_controller(motion);
    view
}

/// Replace what `view` shows with `text` rendered.
pub fn render(view: &gtk4::TextView, text: &str) {
    let buffer = gtk4::TextBuffer::new(None);
    add_tags(&buffer);
    let mut writer = Writer::new(buffer.clone());
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(text, options) {
        writer.event(event);
    }
    writer.finish();
    view.set_buffer(Some(&buffer));
}

/// The web link under the view's `(x, y)`, if any; never another scheme.
fn link_at(view: &gtk4::TextView, x: f64, y: f64) -> Option<String> {
    let (x, y) = view.window_to_buffer_coords(gtk4::TextWindowType::Widget, x as i32, y as i32);
    let iter = view.iter_at_location(x, y)?;
    iter.tags().into_iter().find_map(|tag| {
        let target = tag.name()?.strip_prefix(LINK)?.to_string();
        web_link(&target)
    })
}

/// `target` when it is a plain http(s) link, by the terminal's own rules.
fn web_link(target: &str) -> Option<String> {
    crate::terminal_links::clean_link(target).filter(|url| url == target)
}

fn add_tags(buffer: &gtk4::TextBuffer) {
    let theme = crate::theme::current_theme();
    let rgba = |hex: &str, alpha: f32| {
        let mut color = gdk::RGBA::parse(hex).unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));
        color.set_alpha(alpha);
        color
    };
    let dim = rgba(&theme.foreground, 0.65);
    let shade = rgba(&theme.foreground, 0.10);
    let link = rgba(&theme.accent, 1.0);
    let table = buffer.tag_table();
    let add = |tag: gtk4::TextTag| {
        table.add(&tag);
    };
    for (level, scale) in [(1, 1.8), (2, 1.5), (3, 1.3), (4, 1.15), (5, 1.05), (6, 1.0)] {
        add(gtk4::TextTag::builder()
            .name(format!("h{level}"))
            .weight(700)
            .scale(scale)
            .pixels_above_lines(4)
            .build());
    }
    add(gtk4::TextTag::builder().name("bold").weight(700).build());
    add(gtk4::TextTag::builder().name("italic").style(gtk4::pango::Style::Italic).build());
    add(gtk4::TextTag::builder().name("strike").strikethrough(true).build());
    add(gtk4::TextTag::builder()
        .name("code")
        .family("monospace")
        .background_rgba(&shade)
        .build());
    add(gtk4::TextTag::builder()
        .name("codeblock")
        .family("monospace")
        .paragraph_background_rgba(&shade)
        .wrap_mode(gtk4::WrapMode::Char)
        .build());
    add(gtk4::TextTag::builder()
        .name("quote")
        .style(gtk4::pango::Style::Italic)
        .foreground_rgba(&dim)
        .build());
    add(gtk4::TextTag::builder().name("dim").foreground_rgba(&dim).build());
    add(gtk4::TextTag::builder().name("table").family("monospace").wrap_mode(gtk4::WrapMode::None).build());
    add(gtk4::TextTag::builder()
        .name("link-style")
        .foreground_rgba(&link)
        .underline(gtk4::pango::Underline::Single)
        .build());
}

struct Table {
    rows: Vec<(Vec<String>, bool)>,
    row: Vec<String>,
    cell: String,
}

/// Turns parser events into tagged text, one block after another.
struct Writer {
    buffer: gtk4::TextBuffer,
    /// Names of the tags the text being written gets, innermost last.
    styles: Vec<String>,
    /// The open lists, each with its next number (`None` for bullets).
    lists: Vec<Option<u64>>,
    quotes: usize,
    /// A list item's bullet or number, written with the item's first text.
    marker: Option<String>,
    /// The current item has written nothing yet: its first block starts on
    /// the marker's line.
    item_start: bool,
    table: Option<Table>,
    images: Vec<String>,
}

impl Writer {
    fn new(buffer: gtk4::TextBuffer) -> Self {
        Self {
            buffer,
            styles: Vec::new(),
            lists: Vec::new(),
            quotes: 0,
            marker: None,
            item_start: false,
            table: None,
            images: Vec::new(),
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => self.text(&text),
            Event::Code(text) => {
                self.styles.push("code".into());
                self.text(&text);
                self.styles.pop();
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => self.text(&text),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\n"),
            Event::Rule => {
                self.block();
                self.styles.push("dim".into());
                self.text("────────────────────────────────");
                self.styles.pop();
                self.line();
            }
            Event::TaskListMarker(checked) => {
                self.marker = Some(if checked { "☑ " } else { "☐ " }.into());
            }
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock | Tag::MetadataBlock(_) => self.block(),
            Tag::Heading { level, .. } => {
                self.block();
                let level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                self.styles.push(format!("h{level}"));
            }
            Tag::BlockQuote(_) => {
                self.block();
                self.quotes += 1;
                self.styles.push("quote".into());
            }
            Tag::CodeBlock(_) => {
                self.block();
                self.styles.push("codeblock".into());
            }
            Tag::List(first) => {
                if self.lists.is_empty() {
                    self.block();
                } else {
                    self.line();
                }
                self.lists.push(first);
            }
            Tag::Item => {
                self.line();
                let depth = self.lists.len();
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        *number += 1;
                        format!("{}. ", *number - 1)
                    }
                    _ => format!("{} ", ["•", "◦", "▪"][(depth.max(1) - 1) % 3]),
                };
                self.marker = Some(marker);
                self.item_start = true;
            }
            Tag::Emphasis => self.styles.push("italic".into()),
            Tag::Strong => self.styles.push("bold".into()),
            Tag::Strikethrough => self.styles.push("strike".into()),
            Tag::Link { dest_url, .. } => {
                let name = format!("{LINK}{dest_url}");
                if self.buffer.tag_table().lookup(&name).is_none() {
                    self.buffer.tag_table().add(&gtk4::TextTag::builder().name(&name).build());
                }
                self.styles.push("link-style".into());
                self.styles.push(name);
            }
            Tag::Image { dest_url, .. } => {
                self.styles.push("dim".into());
                self.text("[image: ");
                self.images.push(dest_url.to_string());
            }
            Tag::Table(_) => {
                self.block();
                self.table = Some(Table { rows: Vec::new(), row: Vec::new(), cell: String::new() });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cell.clear();
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) | TagEnd::CodeBlock => {
                self.styles.pop();
                self.line();
            }
            TagEnd::BlockQuote(_) => {
                self.styles.pop();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.lists.pop();
            }
            TagEnd::Item => {
                // An empty item still shows its bullet.
                self.flush_marker();
                self.item_start = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                self.styles.pop();
            }
            TagEnd::Image => {
                let target = self.images.pop().unwrap_or_default();
                self.text(&format!("] {target}"));
                self.styles.pop();
            }
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    let cell = std::mem::take(&mut table.cell);
                    table.row.push(cell.trim().to_string());
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push((row, tag == TagEnd::TableHead));
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.write_table(table);
                }
            }
            _ => {}
        }
    }

    /// The tag that indents text at the current list and quote depth.
    fn indent(&self) -> Option<String> {
        let depth = self.lists.len() + self.quotes;
        if depth == 0 {
            return None;
        }
        let hanging = !self.lists.is_empty();
        let name = format!("indent-{depth}-{hanging}");
        if self.buffer.tag_table().lookup(&name).is_none() {
            let tag = gtk4::TextTag::builder()
                .name(&name)
                .left_margin(12 + 24 * depth as i32)
                .indent(if hanging { -16 } else { 0 })
                .build();
            self.buffer.tag_table().add(&tag);
        }
        Some(name)
    }

    fn put(&self, text: &str, styles: &[String]) {
        let table = self.buffer.tag_table();
        let tags: Vec<gtk4::TextTag> = styles
            .iter()
            .cloned()
            .chain(self.indent())
            .filter_map(|name| table.lookup(&name))
            .collect();
        let tags: Vec<&gtk4::TextTag> = tags.iter().collect();
        let mut end = self.buffer.end_iter();
        self.buffer.insert_with_tags(&mut end, text, &tags);
    }

    fn flush_marker(&mut self) {
        if let Some(marker) = self.marker.take() {
            self.put(&marker, &["bold".into()]);
        }
    }

    fn text(&mut self, text: &str) {
        if let Some(table) = &mut self.table {
            table.cell.push_str(text);
            return;
        }
        self.flush_marker();
        self.item_start = false;
        self.put(text, &self.styles.clone());
    }

    /// Make the buffer end in at least `count` newlines (none at its start).
    fn newlines(&self, count: usize) {
        let end = self.buffer.end_iter();
        let mut from = end;
        from.backward_chars(count as i32);
        let tail = self.buffer.text(&from, &end, false);
        if tail.is_empty() {
            return;
        }
        let have = tail.chars().rev().take_while(|c| *c == '\n').count();
        if have < count {
            let mut end = self.buffer.end_iter();
            self.buffer.insert(&mut end, &"\n".repeat(count - have));
        }
    }

    /// Start a new line.
    fn line(&mut self) {
        self.flush_marker();
        self.newlines(1);
    }

    /// Start a new block, a blank line below the last one; an item's first
    /// block stays on its marker's line.
    fn block(&mut self) {
        if self.item_start {
            self.item_start = false;
            return;
        }
        self.flush_marker();
        self.newlines(if self.lists.is_empty() { 2 } else { 1 });
    }

    fn write_table(&mut self, table: Table) {
        let columns = table.rows.iter().map(|(row, _)| row.len()).max().unwrap_or(0);
        let mut widths = vec![0usize; columns];
        for (row, _) in &table.rows {
            for (column, cell) in row.iter().enumerate() {
                widths[column] = widths[column].max(cell.chars().count());
            }
        }
        for (row, head) in &table.rows {
            let line: Vec<String> = (0..columns)
                .map(|column| {
                    let cell = row.get(column).map_or("", String::as_str);
                    format!("{cell}{}", " ".repeat(widths[column] - cell.chars().count()))
                })
                .collect();
            let mut styles = vec!["table".to_string()];
            if *head {
                styles.push("bold".into());
            }
            self.put(&format!("{}\n", line.join(" │ ").trim_end()), &styles);
            if *head {
                let rule: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
                self.put(&format!("{}\n", rule.join("─┼─")), &["table".into(), "dim".into()]);
            }
        }
    }

    fn finish(&mut self) {
        self.flush_marker();
        // No blank space below the last block.
        let mut end = self.buffer.end_iter();
        let mut from = end;
        while from.backward_char() && from.char() == '\n' {}
        if from.char() != '\n' {
            from.forward_char();
        }
        self.buffer.delete(&mut from, &mut end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(view: &gtk4::TextView) -> String {
        let buffer = view.buffer();
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
    }

    /// Names of the tags at the first occurrence of `needle`.
    fn tags_at(view: &gtk4::TextView, needle: &str) -> Vec<String> {
        let buffer = view.buffer();
        let (start, _) = buffer
            .start_iter()
            .forward_search(needle, gtk4::TextSearchFlags::empty(), None)
            .unwrap_or_else(|| panic!("{needle:?} not rendered"));
        start.tags().into_iter().filter_map(|tag| tag.name().map(|n| n.to_string())).collect()
    }

    #[test]
    fn only_plain_web_links_open() {
        assert_eq!(web_link("https://example.org/a"), Some("https://example.org/a".into()));
        for target in ["file:///etc/passwd", "javascript:alert(1)", "docs/plan.md", "https://user:pw@example.org/", "mailto:a@b"] {
            assert_eq!(web_link(target), None, "{target}");
        }
    }

    #[test]
    fn renders_markdown_as_formatted_text() {
        if !crate::gtk_test::is_child() {
            crate::gtk_test::run_in_child_process("markdown_view::tests::renders_markdown_as_formatted_text");
            return;
        }
        if gtk4::init().is_err() {
            return;
        }
        let view = view(concat!(
            "# Title\n\nSome **bold**, *italic*, ~~gone~~ and `code`.\n\n",
            "- one\n- two\n  - nested\n\n1. first\n2. second\n\n",
            "- [x] done\n- [ ] todo\n\n> quoted\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "| Name | Size |\n|---|---|\n| a | 10 |\n\n",
            "[site](https://example.org/) and [local](docs/plan.md)\n\n---\n\n",
            "<script>ignored</script>\n\n![diagram](https://example.org/image.png)\n",
        ));
        let text = rendered(&view);
        // Markup is gone, the words stay.
        for gone in ["# Title", "**bold**", "*italic*", "~~gone~~", "`code`", "```", "[site]", "- one", "|---|"] {
            assert!(!text.contains(gone), "{gone:?} left in {text:?}");
        }
        assert!(text.starts_with("Title\n\nSome bold, italic, gone and code."), "{text:?}");
        assert!(tags_at(&view, "Title").contains(&"h1".into()));
        assert!(tags_at(&view, "bold,").contains(&"bold".into()));
        assert!(tags_at(&view, "italic,").contains(&"italic".into()));
        assert!(tags_at(&view, "gone").contains(&"strike".into()));
        assert!(tags_at(&view, "code.").contains(&"code".into()));
        assert!(text.contains("• one\n• two\n◦ nested"), "{text:?}");
        assert!(text.contains("1. first\n2. second"), "{text:?}");
        assert!(text.contains("☑ done\n☐ todo"), "{text:?}");
        assert!(tags_at(&view, "quoted").contains(&"quote".into()));
        assert!(tags_at(&view, "fn main").contains(&"codeblock".into()));
        assert!(text.contains("Name │ Size\n───"), "{text:?}");
        assert!(text.contains("a    │ 10"), "{text:?}");
        let site = tags_at(&view, "site");
        assert!(site.contains(&format!("{LINK}https://example.org/")));
        assert!(site.contains(&"link-style".into()));
        assert!(tags_at(&view, "local").contains(&format!("{LINK}docs/plan.md")));
        assert!(text.contains("────"));
        // HTML is inert text, and an image is never loaded: its address shows.
        assert!(text.contains("<script>ignored</script>"));
        assert!(text.contains("[image: diagram] https://example.org/image.png"), "{text:?}");
        // Rendering again replaces, never appends.
        render(&view, "Just *this*");
        assert_eq!(rendered(&view), "Just this");
    }
}
