//! Result export and value inspection helpers.

type Rows = [Vec<Option<String>>];

fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

/// RFC 4180 style CSV with a header row; NULL becomes an empty field.
pub fn to_csv(columns: &[String], rows: &Rows) -> String {
    let mut out = String::new();
    let line = |fields: Vec<String>, out: &mut String| {
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    };
    line(columns.iter().map(|c| csv_field(c)).collect(), &mut out);
    for row in rows {
        line(
            row.iter()
                .map(|v| v.as_deref().map(csv_field).unwrap_or_default())
                .collect(),
            &mut out,
        );
    }
    out
}

/// Array of objects. Values are strings (the text protocol carries no types); NULL is `null`.
pub fn to_json(columns: &[String], rows: &Rows) -> String {
    let objects: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let map = columns
                .iter()
                .zip(row)
                .map(|(c, v)| {
                    (
                        c.clone(),
                        v.as_ref()
                            .map_or(serde_json::Value::Null, |s| s.clone().into()),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            serde_json::Value::Object(map)
        })
        .collect();
    serde_json::to_string_pretty(&objects).unwrap_or_else(|_| "[]".into())
}

/// Characters of a value the inspector shows.
pub const INSPECT_CHARS: usize = 20_000;

/// Largest value that is parsed for JSON pretty-printing.
const PRETTY_MAX_BYTES: usize = 1_000_000;

/// A value prepared for display in the inspector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspected {
    pub text: String,
    /// The shown text is only the start of the value.
    pub shortened: bool,
}

/// Prepares a value for the inspector: pretty-prints JSON (when not huge) and caps the length.
pub fn inspect_text(v: &str) -> Inspected {
    let text = if v.len() <= PRETTY_MAX_BYTES {
        pretty_value(v)
    } else {
        v.to_string()
    };
    match text.char_indices().nth(INSPECT_CHARS) {
        Some((end, _)) => Inspected {
            text: text[..end].to_string(),
            shortened: true,
        },
        None => Inspected {
            text,
            shortened: false,
        },
    }
}

/// A single-line preview of at most `max` characters, for grid cells: line breaks become
/// spaces and longer text ends with an ellipsis.
pub fn preview(v: &str, max: usize) -> String {
    let mut out = String::new();
    for (n, c) in v.chars().enumerate() {
        if n == max {
            out.push('…');
            break;
        }
        out.push(if c == '\n' || c == '\r' || c == '\t' {
            ' '
        } else {
            c
        });
    }
    out
}

/// Pretty-prints JSON objects and arrays; anything else is returned unchanged.
pub fn pretty_value(v: &str) -> String {
    let t = v.trim_start();
    if t.starts_with(['{', '['])
        && let Ok(j) = serde_json::from_str::<serde_json::Value>(v)
        && let Ok(p) = serde_json::to_string_pretty(&j)
    {
        return p;
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<String> {
        vec!["id".into(), "note".into()]
    }

    fn rows() -> Vec<Vec<Option<String>>> {
        vec![
            vec![Some("1".into()), Some("a,b".into())],
            vec![Some("2".into()), None],
            vec![Some("3".into()), Some("say \"hi\"\nnow".into())],
        ]
    }

    #[test]
    fn csv_quotes_and_nulls() {
        assert_eq!(
            to_csv(&cols(), &rows()),
            "id,note\r\n1,\"a,b\"\r\n2,\r\n3,\"say \"\"hi\"\"\nnow\"\r\n"
        );
        assert_eq!(to_csv(&cols(), &[]), "id,note\r\n");
    }

    #[test]
    fn json_objects() {
        let v: serde_json::Value = serde_json::from_str(&to_json(&cols(), &rows())).unwrap();
        assert_eq!(v[0]["note"], "a,b");
        assert!(v[1]["note"].is_null());
        assert_eq!(v[2]["id"], "3");
        assert_eq!(to_json(&cols(), &[]), "[]");
    }

    #[test]
    fn inspect_caps_long_values() {
        let short = inspect_text("{\"a\":1}");
        assert_eq!(short.text, "{\n  \"a\": 1\n}");
        assert!(!short.shortened);
        let long = "x".repeat(INSPECT_CHARS + 5);
        let i = inspect_text(&long);
        assert!(i.shortened);
        assert_eq!(i.text.chars().count(), INSPECT_CHARS);
        let exact = "y".repeat(INSPECT_CHARS);
        assert!(!inspect_text(&exact).shortened);
        let huge = format!("[{}]", "1,".repeat(600_000));
        assert!(
            inspect_text(&huge).shortened,
            "huge JSON is not parsed, only cut"
        );
    }

    #[test]
    fn previews_are_single_line_and_short() {
        assert_eq!(preview("a\nb\r\nc\td", 50), "a b  c d");
        assert_eq!(preview("abcdef", 3), "abc…");
        assert_eq!(preview("abc", 3), "abc");
        assert_eq!(preview("åäöxyz", 3), "åäö…");
        assert_eq!(preview("", 3), "");
    }

    #[test]
    fn pretty_only_for_json_containers() {
        assert_eq!(pretty_value("{\"a\":1}"), "{\n  \"a\": 1\n}");
        assert_eq!(pretty_value("[1,2]"), "[\n  1,\n  2\n]");
        assert_eq!(pretty_value("{not json"), "{not json");
        assert_eq!(pretty_value("42"), "42");
        assert_eq!(pretty_value("plain"), "plain");
    }
}
