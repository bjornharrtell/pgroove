//! Splitting a script into statements, and a peek at what kind of statement each is.

/// One statement of a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The statement without its terminating semicolon.
    pub text: String,
    /// Byte offset of `text` in the script.
    pub start: usize,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Splits on semicolons that are not inside quotes, comments or dollar-quoted bodies.
/// Statements that are empty or only comments are dropped.
pub fn split_statements(sql: &str) -> Vec<Statement> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let push = |from: usize, to: usize, out: &mut Vec<Statement>| {
        let slice = &sql[from..to];
        let trimmed = slice.trim();
        if trimmed.is_empty() || first_keyword(trimmed).is_none() && only_comments(trimmed) {
            return;
        }
        let lead = slice.len() - slice.trim_start().len();
        out.push(Statement {
            text: trimmed.to_string(),
            start: from + lead,
        });
    };
    while i < b.len() {
        match b[i] {
            b'\'' => {
                // E'..' strings treat backslash as an escape.
                let escape =
                    i > 0 && matches!(b[i - 1], b'e' | b'E') && (i < 2 || !is_ident_byte(b[i - 2]));
                i += 1;
                while i < b.len() {
                    if escape && b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == b'\'' {
                        if b.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'"' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'"' {
                        if b.get(i + 1) == Some(&b'"') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'$' => {
                // $tag$ ... $tag$ (the tag may be empty); `$1` parameters are not tags.
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                let is_tag =
                    b.get(j) == Some(&b'$') && !b[i + 1..j].first().is_some_and(u8::is_ascii_digit);
                if is_tag && (i == 0 || !is_ident_byte(b[i - 1])) {
                    let tag = &sql[i..=j];
                    match sql[j + 1..].find(tag) {
                        Some(off) => i = j + 1 + off + tag.len(),
                        None => i = b.len(),
                    }
                } else {
                    i += 1;
                }
            }
            b';' => {
                push(start, i, &mut out);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    push(start, b.len(), &mut out);
    out
}

fn only_comments(text: &str) -> bool {
    let mut rest = text.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, tail)| tail).trim_start();
        } else {
            return rest.is_empty();
        }
    }
}

/// The first keyword of a statement, lower-cased, skipping comments and opening parentheses.
pub fn first_keyword(sql: &str) -> Option<String> {
    let mut rest = sql.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(r) = rest.strip_prefix('(') {
            rest = r.trim_start();
        } else {
            break;
        }
    }
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
        .collect();
    (!word.is_empty()).then(|| word.to_lowercase())
}

/// Whether a statement is a query whose rows can be limited by wrapping it in a subquery.
pub fn is_select_like(sql: &str) -> bool {
    matches!(
        first_keyword(sql).as_deref(),
        Some("select" | "with" | "values" | "table")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<String> {
        split_statements(sql).into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn splits_on_semicolons() {
        assert_eq!(
            texts("select 1; select 2;\n\nselect 3"),
            ["select 1", "select 2", "select 3"]
        );
        assert_eq!(texts(""), Vec::<String>::new());
        assert_eq!(texts(" ; ;; "), Vec::<String>::new());
        let s = split_statements("select 1;  select 2");
        assert_eq!((s[0].start, s[1].start), (0, 11));
    }

    #[test]
    fn ignores_semicolons_in_quotes_comments_and_bodies() {
        assert_eq!(
            texts("select 'a;b'; select 2"),
            ["select 'a;b'", "select 2"]
        );
        assert_eq!(texts("select 'it''s;'; x"), ["select 'it''s;'", "x"]);
        assert_eq!(
            texts(r#"select "a;b" from t; y"#),
            [r#"select "a;b" from t"#, "y"]
        );
        assert_eq!(
            texts("select 1 -- a;b\n; select 2"),
            ["select 1 -- a;b", "select 2"]
        );
        assert_eq!(
            texts("select /* ; /* nested ; */ ; */ 1; z"),
            ["select /* ; /* nested ; */ ; */ 1", "z"]
        );
        assert_eq!(
            texts("create function f() returns int as $$ select 1; $$ language sql; select 2"),
            [
                "create function f() returns int as $$ select 1; $$ language sql",
                "select 2"
            ]
        );
        assert_eq!(
            texts("do $body$ begin null; end $body$; x"),
            ["do $body$ begin null; end $body$", "x"]
        );
        assert_eq!(texts(r"select E'a\';b'; q"), [r"select E'a\';b'", "q"]);
        assert_eq!(texts("select $1; select 2"), ["select $1", "select 2"]);
    }

    #[test]
    fn drops_comment_only_statements() {
        assert_eq!(texts("-- nothing\n; select 1; /* c */"), ["select 1"]);
        assert_eq!(texts("-- a\nselect 1"), ["-- a\nselect 1"]);
    }

    #[test]
    fn classifies_statements() {
        for s in [
            "select 1",
            "  WITH x as (select 1) select * from x",
            "values (1)",
            "table t",
            "(select 1)",
            "-- c\nselect 1",
            "/* c */ select 1",
        ] {
            assert!(is_select_like(s), "{s}");
        }
        for s in [
            "insert into t values (1)",
            "update t set a=1",
            "explain select 1",
            "show all",
            "create table t()",
            "",
            "123",
        ] {
            assert!(!is_select_like(s), "{s}");
        }
        assert_eq!(first_keyword("  ( Select 1)"), Some("select".into()));
        assert_eq!(first_keyword("--only"), None);
    }
}
