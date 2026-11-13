//! Splitting a script into statements, and a peek at what kind of statement each is.

/// One statement of a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The statement without its terminating semicolon.
    pub text: String,
    /// Byte offset of `text` in the script.
    pub start: usize,
}

/// What a run command covers when nothing is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunScope {
    /// The statement under the cursor (Run).
    Statement,
    /// The whole text (Run script).
    Script,
}

/// The statement the cursor is in: the last one that starts at or before it, so a cursor in
/// the blank space after a statement (or right after its semicolon) still means that
/// statement; before the first statement it means the first.
pub fn statement_at(sql: &str, cursor: usize) -> Option<Statement> {
    let all = split_statements(sql);
    let ix = all.iter().rposition(|s| s.start <= cursor).unwrap_or(0);
    all.into_iter().nth(ix)
}

/// The SQL a run command sends and the byte offset where it starts in the text: the selection
/// when there is one, else what `scope` says. `selection` is the editor's selected byte range
/// (empty when only a cursor).
pub fn sql_to_run(
    text: &str,
    selection: std::ops::Range<usize>,
    scope: RunScope,
) -> (String, usize) {
    let selected = text.get(selection.clone()).unwrap_or("");
    if !selected.trim().is_empty() {
        return (selected.to_string(), selection.start);
    }
    if scope == RunScope::Statement
        && let Some(s) = statement_at(text, selection.start)
    {
        return (s.text, s.start);
    }
    (text.to_string(), 0)
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

    const SCRIPT: &str = "select 1;\n\nselect 2 from t;\nselect 3";

    fn at(cursor: usize) -> String {
        statement_at(SCRIPT, cursor).unwrap().text
    }

    #[test]
    fn cursor_picks_its_statement() {
        assert_eq!(at(0), "select 1");
        assert_eq!(at(5), "select 1");
        assert_eq!(at(8), "select 1", "just before the semicolon");
        assert_eq!(at(9), "select 1", "right after the semicolon");
        assert_eq!(at(10), "select 1", "blank line after a statement");
        let second = SCRIPT.find("select 2").unwrap();
        assert_eq!(at(second), "select 2 from t");
        assert_eq!(at(second + 12), "select 2 from t");
        let third = SCRIPT.find("select 3").unwrap();
        assert_eq!(
            at(third - 1),
            "select 2 from t",
            "the newline before the next statement"
        );
        assert_eq!(at(third), "select 3");
        assert_eq!(at(SCRIPT.len()), "select 3", "end of text");
        assert_eq!(at(10_000), "select 3", "past the end");
    }

    #[test]
    fn cursor_before_the_first_statement_means_the_first() {
        let sql = "  \n  select 1; select 2";
        assert_eq!(statement_at(sql, 0).unwrap().text, "select 1");
        assert_eq!(statement_at("", 0), None);
        assert_eq!(statement_at(" ; -- note", 3), None);
    }

    #[test]
    fn cursor_ignores_semicolons_in_strings() {
        let sql = "select 'a;b'; select 2";
        assert_eq!(statement_at(sql, 9).unwrap().text, "select 'a;b'");
        assert_eq!(statement_at(sql, 16).unwrap().text, "select 2");
    }

    #[test]
    fn selection_wins_over_scope() {
        for scope in [RunScope::Statement, RunScope::Script] {
            let (sql, base) = sql_to_run(SCRIPT, 3..8, scope);
            assert_eq!((sql.as_str(), base), ("ect 1", 3));
        }
        // A selection of only blanks counts as no selection.
        let (sql, base) = sql_to_run(SCRIPT, 9..11, RunScope::Script);
        assert_eq!((sql.as_str(), base), (SCRIPT, 0));
    }

    #[test]
    fn scope_decides_without_a_selection() {
        let second = SCRIPT.find("select 2").unwrap();
        let (sql, base) = sql_to_run(SCRIPT, second + 3..second + 3, RunScope::Statement);
        assert_eq!((sql.as_str(), base), ("select 2 from t", second));
        let (sql, base) = sql_to_run(SCRIPT, second + 3..second + 3, RunScope::Script);
        assert_eq!((sql.as_str(), base), (SCRIPT, 0));
        // Nothing to pick: the text is sent as it is (and an empty text sends nothing).
        let (sql, base) = sql_to_run("  ", 1..1, RunScope::Statement);
        assert_eq!((sql.as_str(), base), ("  ", 0));
        // A range that is not on character boundaries falls back instead of panicking.
        let (sql, _) = sql_to_run("é; select 1", 1..1, RunScope::Script);
        assert_eq!(sql, "é; select 1");
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
