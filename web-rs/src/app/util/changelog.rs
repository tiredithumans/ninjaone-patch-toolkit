//! A tiny Markdown subset renderer for the update splash's release notes.

/// One inline run within a changelog line: plain text or a `**bold**` span.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MdSpan {
    Text(String),
    Strong(String),
}

/// One rendered block of the update changelog (a `CHANGELOG.md` version section).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MdBlock {
    /// A `#`/`##`/`###` section heading (e.g. "Added", "Fixed").
    Heading(String),
    /// A bullet list; each item is its sequence of inline spans.
    List(Vec<Vec<MdSpan>>),
    /// A free-text paragraph (the GitHub fallback note, or any non-list text).
    Paragraph(Vec<MdSpan>),
}

/// Splits one line into `**bold**` and plain-text runs. An unterminated `**` is
/// left as literal text so we never drop content.
pub(crate) fn parse_inline(text: &str) -> Vec<MdSpan> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("**") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("**") else {
            break; // no closing marker — the rest is plain text
        };
        if open > 0 {
            spans.push(MdSpan::Text(rest[..open].to_string()));
        }
        let bold = &after[..close];
        if !bold.is_empty() {
            spans.push(MdSpan::Strong(bold.to_string()));
        }
        rest = &after[close + 2..];
    }
    if !rest.is_empty() {
        spans.push(MdSpan::Text(rest.to_string()));
    }
    spans
}

/// Parses the changelog subset the updater notes use — `#` headings, `-`/`*` bullet
/// lists (wrapped continuation lines fold into the bullet), `**bold**`, and plain
/// paragraphs — into renderable blocks. Anything unrecognized falls through as text,
/// so the GitHub fallback note ("See the release notes …") renders as a paragraph.
pub(crate) fn parse_changelog(src: &str) -> Vec<MdBlock> {
    let mut blocks = Vec::new();
    let mut items: Vec<String> = Vec::new(); // raw text of the bullets in the open list
    let mut para: Vec<String> = Vec::new(); // raw lines of the open paragraph

    for raw in src.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            flush_para(&mut blocks, &mut para);
            flush_list(&mut blocks, &mut items);
        } else if trimmed.starts_with('#') {
            flush_para(&mut blocks, &mut para);
            flush_list(&mut blocks, &mut items);
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                blocks.push(MdBlock::Heading(heading.to_string()));
            }
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush_para(&mut blocks, &mut para); // a list and paragraph never overlap
            items.push(item.trim().to_string());
        } else if let Some(last) = items.last_mut() {
            // A non-blank, non-marker line under a bullet is a wrapped continuation.
            last.push(' ');
            last.push_str(trimmed);
        } else {
            para.push(trimmed.to_string());
        }
    }
    flush_para(&mut blocks, &mut para);
    flush_list(&mut blocks, &mut items);
    blocks
}

fn flush_list(blocks: &mut Vec<MdBlock>, items: &mut Vec<String>) {
    if !items.is_empty() {
        let spans = items.drain(..).map(|i| parse_inline(&i)).collect();
        blocks.push(MdBlock::List(spans));
    }
}

fn flush_para(blocks: &mut Vec<MdBlock>, para: &mut Vec<String>) {
    if !para.is_empty() {
        let text = para.join(" ");
        para.clear();
        blocks.push(MdBlock::Paragraph(parse_inline(&text)));
    }
}
