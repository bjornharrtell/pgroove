//! Which identifiers need double quotes in generated SQL.

/// Words PostgreSQL refuses as plain identifiers: its reserved keywords and the
/// keywords that cannot be column or function names (`pg_get_keywords()` with a
/// category other than `U`), as of PostgreSQL 18. Sorted, for binary search.
const KEYWORDS: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "between",
    "bigint",
    "binary",
    "bit",
    "boolean",
    "both",
    "case",
    "cast",
    "char",
    "character",
    "check",
    "coalesce",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "dec",
    "decimal",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "exists",
    "extract",
    "false",
    "fetch",
    "float",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "greatest",
    "group",
    "grouping",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "inout",
    "int",
    "integer",
    "intersect",
    "interval",
    "into",
    "is",
    "isnull",
    "join",
    "json",
    "json_array",
    "json_arrayagg",
    "json_exists",
    "json_object",
    "json_objectagg",
    "json_query",
    "json_scalar",
    "json_serialize",
    "json_table",
    "json_value",
    "lateral",
    "leading",
    "least",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "merge_action",
    "national",
    "natural",
    "nchar",
    "none",
    "normalize",
    "not",
    "notnull",
    "null",
    "nullif",
    "numeric",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "out",
    "outer",
    "overlaps",
    "overlay",
    "placing",
    "position",
    "precision",
    "primary",
    "real",
    "references",
    "returning",
    "right",
    "row",
    "select",
    "session_user",
    "setof",
    "similar",
    "smallint",
    "some",
    "substring",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "time",
    "timestamp",
    "to",
    "trailing",
    "treat",
    "trim",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "values",
    "varchar",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
    "xmlattributes",
    "xmlconcat",
    "xmlelement",
    "xmlexists",
    "xmlforest",
    "xmlnamespaces",
    "xmlparse",
    "xmlpi",
    "xmlroot",
    "xmlserialize",
    "xmltable",
];

/// Whether `s` has to be quoted to mean the same identifier: it is not a lowercase
/// letter or underscore followed by lowercase letters, digits and underscores, or it
/// is a keyword. This is the rule of the server's `quote_ident`.
pub fn needs_quotes(s: &str) -> bool {
    let mut chars = s.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    !plain || KEYWORDS.binary_search(&s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_stay_bare() {
        for s in ["orders", "_x", "a1", "order_items", "a_1_b"] {
            assert!(!needs_quotes(s), "{s}");
        }
    }

    #[test]
    fn other_names_are_quoted() {
        for s in [
            "", "Orders", "1a", "a b", "a-b", "a$1", "é", "a\"b", "select", "user", "order", "int",
            "table",
        ] {
            assert!(needs_quotes(s), "{s:?}");
        }
    }

    #[test]
    fn keywords_are_sorted() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
    }

    /// The rule never leaves bare a name the server would quote. Servers older than the one
    /// the keyword list was taken from may bare words we quote, which is harmless, so a
    /// keyword is only compared one way; other names must agree exactly.
    #[tokio::test]
    async fn agrees_with_the_server() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        let words: Vec<String> = client
            .query("select word from pg_get_keywords()", &[])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        let plain =
            ["orders", "Orders", "_x", "1a", "a b", "a$1", "é", "x_1", ""].map(String::from);
        for (name, keyword) in words
            .into_iter()
            .map(|w| (w, true))
            .chain(plain.into_iter().map(|w| (w, false)))
        {
            let quoted: String = client
                .query_one("select quote_ident($1)", &[&name])
                .await
                .unwrap()
                .get(0);
            let server = quoted.starts_with('"');
            if keyword {
                assert!(
                    !server || needs_quotes(&name),
                    "{name:?} is bare but the server quotes it"
                );
            } else {
                assert_eq!(needs_quotes(&name), server, "{name:?}");
            }
        }
    }
}
