use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};
use regex::Regex;
use std::sync::LazyLock;

// --- Regex patterns ---

static JSON_NUMBER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?\b").unwrap());

static JSON_BOOL_NULL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:true|false|null)\b").unwrap());

static LOG_LEVEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(ERROR|FATAL|WARN(?:ING)?|INFO|DEBUG|TRACE)\b").unwrap());

static FILE_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\w./-]+\.\w{1,5}:\d+").unwrap());

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s\]>)]+").unwrap());

// --- Styles ---

const KEY_STYLE: Style = Style::new().fg(Color::Cyan);
const STRING_STYLE: Style = Style::new().fg(Color::Yellow);
const NUMBER_STYLE: Style = Style::new().fg(Color::Magenta);
const BOOL_NULL_STYLE: Style = Style::new().fg(Color::Red);
const STRUCTURAL_STYLE: Style = Style::new().fg(Color::DarkGray);
const URL_STYLE: Style = Style::new().fg(Color::Cyan);
const FILE_LINE_STYLE: Style = Style::new().fg(Color::Yellow);

/// Highlight a line of output text, returning styled spans.
/// Input commands (is_input=true) are left as-is with base_style.
pub fn highlight_line(text: &str, base_style: Style, is_input: bool) -> Vec<Span<'static>> {
    if is_input || text.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let trimmed = text.trim();

    if is_json_like(trimmed) {
        return highlight_json(text);
    }

    highlight_general(text, base_style)
}

fn is_json_like(text: &str) -> bool {
    (text.starts_with('{') || text.starts_with('[') || text.starts_with('"'))
        && (text.contains("\":") || text.ends_with('}') || text.ends_with(']'))
}

// --- JSON highlighting ---

fn highlight_json(text: &str) -> Vec<Span<'static>> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            b'"' => {
                // Find end of quoted string
                let start = i;
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let s = &text[start..i];

                // Check if followed by ':' (skip whitespace) → key
                let mut j = i;
                while j < len && bytes[j] == b' ' {
                    j += 1;
                }
                let is_key = j < len && bytes[j] == b':';

                spans.push(Span::styled(
                    s.to_string(),
                    if is_key { KEY_STYLE } else { STRING_STYLE },
                ));
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                spans.push(Span::styled(
                    text[i..i + 1].to_string(),
                    STRUCTURAL_STYLE,
                ));
                i += 1;
            }
            b' ' | b'\t' => {
                // Collect contiguous whitespace
                let start = i;
                while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                }
                spans.push(Span::raw(text[start..i].to_string()));
            }
            _ => {
                // Collect a word token (number, bool, null, or other)
                let start = i;
                while i < len && !matches!(bytes[i], b'"' | b'{' | b'}' | b'[' | b']' | b':' | b',' | b' ' | b'\t') {
                    i += 1;
                }
                let word = &text[start..i];
                let style = if JSON_BOOL_NULL_RE.is_match(word) {
                    BOOL_NULL_STYLE
                } else if JSON_NUMBER_RE.is_match(word) {
                    NUMBER_STYLE
                } else {
                    STRUCTURAL_STYLE
                };
                spans.push(Span::styled(word.to_string(), style));
            }
        }
    }

    if spans.is_empty() {
        vec![Span::raw(text.to_string())]
    } else {
        spans
    }
}

// --- General highlighting (log levels, file:line, URLs) ---

struct Region {
    start: usize,
    end: usize,
    style: Style,
}

fn highlight_general(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut regions: Vec<Region> = Vec::new();

    // Collect log level matches
    for m in LOG_LEVEL_RE.find_iter(text) {
        let style = match m.as_str() {
            "ERROR" | "FATAL" => Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
            "WARN" | "WARNING" => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            "INFO" => Style::default().fg(Color::Green),
            "DEBUG" | "TRACE" => Style::default().fg(Color::DarkGray),
            _ => base_style,
        };
        regions.push(Region {
            start: m.start(),
            end: m.end(),
            style,
        });
    }

    // Collect URL matches
    for m in URL_RE.find_iter(text) {
        regions.push(Region {
            start: m.start(),
            end: m.end(),
            style: URL_STYLE,
        });
    }

    // Collect file:line matches (skip if overlapping with URL)
    for m in FILE_LINE_RE.find_iter(text) {
        regions.push(Region {
            start: m.start(),
            end: m.end(),
            style: FILE_LINE_STYLE,
        });
    }

    if regions.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    // Sort by start position, then build non-overlapping spans
    regions.sort_by_key(|r| r.start);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0;

    for region in &regions {
        // Skip overlapping regions
        if region.start < cursor {
            continue;
        }

        // Base-styled gap before this region
        if region.start > cursor {
            spans.push(Span::styled(
                text[cursor..region.start].to_string(),
                base_style,
            ));
        }

        spans.push(Span::styled(
            text[region.start..region.end].to_string(),
            region.style,
        ));
        cursor = region.end;
    }

    // Trailing text
    if cursor < text.len() {
        spans.push(Span::styled(text[cursor..].to_string(), base_style));
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    /// Helper: collect span text content from a Vec<Span>.
    fn span_texts<'a>(spans: &'a [Span]) -> Vec<&'a str> {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Helper: find the style of the span containing `needle`.
    fn style_of(spans: &[Span], needle: &str) -> Option<Style> {
        spans.iter().find(|s| s.content.contains(needle)).map(|s| s.style)
    }

    // --- is_json_like ---

    #[test]
    fn json_like_object() {
        assert!(is_json_like(r#"{"key": "value"}"#));
    }

    #[test]
    fn json_like_array() {
        assert!(is_json_like("[1, 2, 3]"));
    }

    #[test]
    fn json_like_quoted_key() {
        assert!(is_json_like(r#""name": "test""#));
    }

    #[test]
    fn not_json_plain_text() {
        assert!(!is_json_like("hello world"));
    }

    #[test]
    fn not_json_empty() {
        assert!(!is_json_like(""));
    }

    // --- highlight_json ---

    #[test]
    fn json_key_vs_string_value() {
        let spans = highlight_json(r#"{"name": "alice"}"#);
        assert_eq!(style_of(&spans, "\"name\""), Some(KEY_STYLE));
        assert_eq!(style_of(&spans, "\"alice\""), Some(STRING_STYLE));
    }

    #[test]
    fn json_number() {
        let spans = highlight_json(r#"{"count": 42}"#);
        assert_eq!(style_of(&spans, "42"), Some(NUMBER_STYLE));
    }

    #[test]
    fn json_bool_and_null() {
        let spans = highlight_json(r#"{"a": true, "b": null}"#);
        assert_eq!(style_of(&spans, "true"), Some(BOOL_NULL_STYLE));
        assert_eq!(style_of(&spans, "null"), Some(BOOL_NULL_STYLE));
    }

    #[test]
    fn json_structural_chars() {
        let spans = highlight_json(r#"{"a": 1}"#);
        assert_eq!(style_of(&spans, "{"), Some(STRUCTURAL_STYLE));
        assert_eq!(style_of(&spans, "}"), Some(STRUCTURAL_STYLE));
        assert_eq!(style_of(&spans, ":"), Some(STRUCTURAL_STYLE));
    }

    #[test]
    fn json_trailing_backslash_no_panic() {
        // This previously caused an out-of-bounds panic
        let spans = highlight_json(r#""value\"#);
        let text: String = span_texts(&spans).join("");
        assert_eq!(text, r#""value\"#);
    }

    #[test]
    fn json_escaped_quote_in_string() {
        let spans = highlight_json(r#"{"msg": "say \"hi\""}"#);
        let text: String = span_texts(&spans).join("");
        assert_eq!(text, r#"{"msg": "say \"hi\""}"#);
    }

    #[test]
    fn json_nested_object() {
        let spans = highlight_json(r#"{"a": {"b": 1}}"#);
        assert_eq!(style_of(&spans, "\"a\""), Some(KEY_STYLE));
        assert_eq!(style_of(&spans, "\"b\""), Some(KEY_STYLE));
        assert_eq!(style_of(&spans, "1"), Some(NUMBER_STYLE));
    }

    // --- highlight_general ---

    #[test]
    fn log_level_error() {
        let base = Style::default();
        let spans = highlight_general("2024-01-01 ERROR something failed", base);
        let error_style = style_of(&spans, "ERROR").unwrap();
        assert_eq!(error_style.fg, Some(Color::Red));
    }

    #[test]
    fn log_level_warn() {
        let base = Style::default();
        let spans = highlight_general("WARNING: disk almost full", base);
        let warn_style = style_of(&spans, "WARNING").unwrap();
        assert_eq!(warn_style.fg, Some(Color::Yellow));
    }

    #[test]
    fn log_level_info_and_debug() {
        let base = Style::default();
        let spans = highlight_general("INFO started | DEBUG details", base);
        assert_eq!(style_of(&spans, "INFO").unwrap().fg, Some(Color::Green));
        assert_eq!(style_of(&spans, "DEBUG").unwrap().fg, Some(Color::DarkGray));
    }

    #[test]
    fn url_highlighted() {
        let base = Style::default();
        let spans = highlight_general("visit https://example.com/path for info", base);
        assert_eq!(
            style_of(&spans, "https://example.com/path"),
            Some(URL_STYLE)
        );
    }

    #[test]
    fn file_line_highlighted() {
        let base = Style::default();
        let spans = highlight_general("error at src/main.rs:42", base);
        assert_eq!(
            style_of(&spans, "src/main.rs:42"),
            Some(FILE_LINE_STYLE)
        );
    }

    #[test]
    fn overlapping_url_and_file_line() {
        let base = Style::default();
        // A URL containing a file:line pattern — URL should win (it's added first)
        let spans = highlight_general("see https://github.com/file.rs:10 here", base);
        // The whole URL region takes precedence
        let url_span = spans.iter().find(|s| s.content.contains("https://"));
        assert!(url_span.is_some());
        assert_eq!(url_span.unwrap().style, URL_STYLE);
    }

    #[test]
    fn plain_text_no_highlights() {
        let base = Style::default().fg(Color::White);
        let spans = highlight_general("just some normal text", base);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, base);
    }

    // --- highlight_line (integration) ---

    #[test]
    fn input_lines_not_highlighted() {
        let base = Style::default().fg(Color::Green);
        let spans = highlight_line(r#"{"key": "value"}"#, base, true);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, base);
    }

    #[test]
    fn empty_line_returns_single_span() {
        let base = Style::default();
        let spans = highlight_line("", base, false);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "");
    }
}
