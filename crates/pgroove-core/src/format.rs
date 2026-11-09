//! SQL formatting.

use sqlformat::{FormatOptions, Indent, QueryParams};

/// Formats SQL with two-space indentation and uppercase keywords.
pub fn format_sql(sql: &str) -> String {
    let opts = FormatOptions {
        indent: Indent::Spaces(2),
        uppercase: Some(true),
        lines_between_queries: 1,
        ..FormatOptions::default()
    };
    sqlformat::format(sql, &QueryParams::None, &opts)
}

#[cfg(test)]
mod tests {
    use super::format_sql;

    #[test]
    fn formats_keywords_and_lines() {
        let out = format_sql("select a,b from t where a=1 and b=2");
        assert!(out.contains("SELECT"));
        assert!(out.contains("\nFROM"));
        assert!(out.contains("\nWHERE"));
        assert_eq!(format_sql(""), "");
    }

    #[test]
    fn keeps_strings_intact() {
        assert!(format_sql("select 'a  b' from t").contains("'a  b'"));
    }
}
