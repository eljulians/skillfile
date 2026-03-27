//! Shared scraping utilities for registry implementations.
//!
//! HTML-to-markdown conversion, entity decoding, JSON string parsing,
//! and URL encoding — reused by both agentskill.sh and skills.sh.

/// Find the byte length of a JSON string literal (including both quotes).
///
/// Input must start with `"`. Returns the position just past the closing
/// `"`, or `None` if unterminated. Safe on UTF-8 because `"` and `\` are
/// single-byte ASCII and cannot appear as continuation bytes.
pub(crate) fn json_string_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn emit_markdown_for_tag(tag: &str, out: &mut String) {
    let name = tag.split_whitespace().next().unwrap_or("");
    match name {
        "h1" => out.push_str("\n# "),
        "h2" => out.push_str("\n## "),
        "h3" => out.push_str("\n### "),
        "h4" => out.push_str("\n#### "),
        "/h1" | "/h2" | "/h3" | "/h4" | "p" | "/p" | "br" | "br/" => out.push('\n'),
        "li" => out.push_str("\n- "),
        "code" | "/code" => out.push('`'),
        _ => {}
    }
}

/// Process a single HTML tag found at `tag_start` in `html`.
///
/// Converts known tags to markdown markers, skips unknown tags.
/// Returns the position after the closing `>`, or `None` if unterminated.
fn process_html_tag(html: &str, tag_start: usize, out: &mut String) -> Option<usize> {
    let end = html[tag_start..].find('>')?;
    let tag = &html[tag_start + 1..tag_start + end];
    emit_markdown_for_tag(tag, out);
    Some(tag_start + end + 1)
}

/// Convert HTML to approximate markdown for TUI preview rendering.
///
/// Converts `<h1>`..`<h4>` to `#` prefixes, `<li>` to `- ` bullets,
/// `<p>` and `<br>` to newlines, `<code>` to backticks.
/// Strips remaining tags and decodes HTML entities.
pub(crate) fn html_to_markdown(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    while pos < html.len() {
        let Some(offset) = html[pos..].find('<') else {
            out.push_str(&html[pos..]);
            break;
        };
        out.push_str(&html[pos..pos + offset]);
        let tag_start = pos + offset;
        if let Some(next) = process_html_tag(html, tag_start, &mut out) {
            pos = next;
        } else {
            out.push_str(&html[tag_start..]);
            break;
        }
    }
    let decoded = html_escape::decode_html_entities(&out);
    collapse_blank_lines(&decoded)
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut consecutive = 0_u8;
    for c in s.chars() {
        let is_newline = c == '\n';
        consecutive = if is_newline {
            consecutive.saturating_add(1)
        } else {
            0
        };
        if !is_newline || consecutive <= 2 {
            out.push(c);
        }
    }
    out.trim_start_matches('\n').to_string()
}

fn percent_encode_char(c: char, out: &mut String) {
    use std::fmt::Write;
    let mut buf = [0u8; 4];
    for byte in c.encode_utf8(&mut buf).as_bytes() {
        let _ = write!(out, "%{byte:02X}");
    }
}

pub(crate) fn urlencoded(s: &str) -> String {
    let s = s.trim();
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push('+'),
            '&' | '=' | '?' | '#' | '+' | '%' => percent_encode_char(c, &mut out),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- json_string_end tests ------------------------------------------------

    #[test]
    fn json_string_end_finds_closing_quote() {
        assert_eq!(json_string_end(r#""hello""#), Some(7));
        assert_eq!(json_string_end(r#""esc\"d""#), Some(8));
        assert_eq!(json_string_end(r#""\\""#), Some(4));
        assert!(json_string_end("not a string").is_none());
        assert!(json_string_end(r#""unterminated"#).is_none());
    }

    // -- html_to_markdown tests -----------------------------------------------

    #[test]
    fn html_to_markdown_strips_unknown_tags_and_decodes() {
        let md = html_to_markdown("<p>hello &amp; world</p>");
        assert!(md.contains("hello & world"), "got: {md}");
        assert_eq!(html_to_markdown("no tags"), "no tags");
    }

    #[test]
    fn html_to_markdown_decodes_hex_entities() {
        let md = html_to_markdown("pod/&#x3C;pod&#x3E; -n &#x3C;ns&#x3E;");
        assert!(md.contains("pod/<pod> -n <ns>"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_converts_headings() {
        let html = "<h1>Title</h1><h2>Section</h2><p>Text</p>";
        let md = html_to_markdown(html);
        assert!(md.contains("# Title"), "got: {md}");
        assert!(md.contains("## Section"), "got: {md}");
        assert!(md.contains("Text"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_converts_lists() {
        let html = "<ul><li>First</li><li>Second</li></ul>";
        let md = html_to_markdown(html);
        assert!(md.contains("- First"), "got: {md}");
        assert!(md.contains("- Second"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_handles_attributes() {
        let html = r#"<h1 class="title">Title</h1>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("# Title"), "got: {md}");
    }

    #[test]
    fn html_to_markdown_decodes_entities() {
        let html = "<p>a &amp; b &#x3C;c&#x3E;</p>";
        let md = html_to_markdown(html);
        assert!(md.contains("a & b <c>"), "got: {md}");
    }

    // -- collapse_blank_lines tests -------------------------------------------

    #[test]
    fn collapse_blank_lines_limits_consecutive() {
        assert_eq!(collapse_blank_lines("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(collapse_blank_lines("\n\n# Title"), "# Title");
        assert_eq!(collapse_blank_lines("a\n\nb"), "a\n\nb");
    }

    #[test]
    fn html_to_markdown_no_excessive_blank_lines() {
        let html = "<h1>Title</h1>\n<p>Paragraph one.</p>\n<p>Paragraph two.</p>";
        let md = html_to_markdown(html);
        assert!(!md.contains("\n\n\n"), "triple newlines in: {md}");
        assert!(md.contains("# Title"), "got: {md}");
        assert!(md.contains("Paragraph one"), "got: {md}");
        assert!(md.contains("Paragraph two"), "got: {md}");
    }

    // -- urlencoded tests -----------------------------------------------------

    #[test]
    fn urlencoded_encodes_spaces_and_specials() {
        assert_eq!(urlencoded("code review"), "code+review");
        assert_eq!(urlencoded("a&b"), "a%26b");
        assert_eq!(urlencoded("q=1"), "q%3D1");
        assert_eq!(urlencoded("hello"), "hello");
        assert_eq!(urlencoded("docker\n"), "docker");
        assert_eq!(urlencoded("  docker  "), "docker");
        assert_eq!(urlencoded("代码审查"), "代码审查");
    }
}
