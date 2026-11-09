//! Lazy-loaded schema tree state and its flattened row model.

use std::collections::{HashMap, HashSet};

use pgroove_core::catalog::{ObjectKind, SchemaObject};

/// Load state of a lazily fetched list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// Groups of objects shown under an expanded schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Tables,
    Views,
    Sequences,
    Types,
    Functions,
}

impl Category {
    pub const ORDER: [Category; 5] = [
        Category::Tables,
        Category::Views,
        Category::Sequences,
        Category::Types,
        Category::Functions,
    ];

    pub fn of(kind: ObjectKind) -> Self {
        match kind {
            ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::ForeignTable => {
                Category::Tables
            }
            ObjectKind::View | ObjectKind::MaterializedView => Category::Views,
            ObjectKind::Sequence => Category::Sequences,
            ObjectKind::EnumType | ObjectKind::CompositeType | ObjectKind::DomainType => {
                Category::Types
            }
            ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Aggregate => {
                Category::Functions
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Tables => "Tables",
            Category::Views => "Views",
            Category::Sequences => "Sequences",
            Category::Types => "Types",
            Category::Functions => "Functions",
        }
    }

    /// Tables start expanded; the other groups start collapsed.
    fn default_open(self) -> bool {
        self == Category::Tables
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Schema {
        name: String,
        expanded: bool,
    },
    Category {
        schema: String,
        category: Category,
        expanded: bool,
        count: usize,
    },
    Object {
        schema: String,
        name: String,
        kind: ObjectKind,
    },
    Message(String),
    ExtensionsHeader {
        expanded: bool,
        count: Option<usize>,
    },
    Extension {
        name: String,
        version: String,
    },
}

#[derive(Default)]
pub struct SchemaTree {
    schemas: Option<Load<Vec<String>>>,
    objects: HashMap<String, Load<Vec<SchemaObject>>>,
    expanded: HashSet<String>,
    /// Groups whose state differs from their default.
    toggled: HashSet<(String, Category)>,
    extensions: Option<Load<Vec<(String, String)>>>,
    extensions_open: bool,
    /// Lower-cased search text; when set, the tree shows matching objects only.
    filter: Option<String>,
    /// Every object of every schema, loaded when a search starts.
    all: Option<Load<Vec<(String, SchemaObject)>>>,
}

impl SchemaTree {
    /// Drops loaded data but keeps what is expanded; returns the expanded schemas to refetch.
    pub fn refresh(&mut self) -> Vec<String> {
        let mut open: Vec<String> = self.expanded.iter().cloned().collect();
        open.sort();
        let keep = std::mem::take(&mut self.expanded);
        let toggled = std::mem::take(&mut self.toggled);
        let filter = self.filter.take();
        let ext_open = self.extensions_open;
        *self = Self::default();
        self.filter = filter;
        self.expanded = keep;
        self.toggled = toggled;
        self.extensions_open = ext_open;
        for name in &open {
            self.objects.insert(name.clone(), Load::Loading);
        }
        open
    }

    pub fn set_extensions(&mut self, result: Result<Vec<(String, String)>, String>) {
        self.extensions = Some(match result {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e),
        });
    }

    /// Expands or collapses a group of objects under a schema.
    pub fn toggle_category(&mut self, schema: &str, category: Category) {
        let key = (schema.to_string(), category);
        if !self.toggled.remove(&key) {
            self.toggled.insert(key);
        }
    }

    fn category_open(&self, schema: &str, category: Category) -> bool {
        category.default_open() != self.toggled.contains(&(schema.to_string(), category))
    }

    /// Sets the search text; an empty text leaves search mode.
    pub fn set_filter(&mut self, text: &str) {
        let t = text.trim().to_lowercase();
        self.filter = (!t.is_empty()).then_some(t);
    }

    /// Whether a search is waiting for the full object list.
    pub fn needs_all_objects(&self) -> bool {
        self.filter.is_some() && self.all.is_none()
    }

    pub fn start_loading_all(&mut self) {
        self.all = Some(Load::Loading);
    }

    pub fn set_all_objects(&mut self, result: Result<Vec<(String, SchemaObject)>, String>) {
        self.all = Some(match result {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e),
        });
    }

    fn filtered_rows(&self, filter: &str) -> Vec<Row> {
        const MAX_SHOWN: usize = 1000;
        let all = match &self.all {
            None | Some(Load::Loading) => return vec![Row::Message("Searching…".into())],
            Some(Load::Failed(e)) => {
                return vec![Row::Message(format!("Couldn't search: {e}"))];
            }
            Some(Load::Ready(v)) => v,
        };
        let (mut rows, mut shown, mut total) = (Vec::new(), 0, 0);
        let mut i = 0;
        while i < all.len() {
            let schema = &all[i].0;
            let end = all[i..]
                .iter()
                .position(|(s, _)| s != schema)
                .map_or(all.len(), |n| i + n);
            let schema_match = schema.to_lowercase().contains(filter);
            let matches: Vec<&SchemaObject> = all[i..end]
                .iter()
                .map(|(_, o)| o)
                .filter(|o| schema_match || o.name.to_lowercase().contains(filter))
                .collect();
            i = end;
            total += matches.len();
            if matches.is_empty() || shown >= MAX_SHOWN {
                continue;
            }
            rows.push(Row::Schema {
                name: schema.clone(),
                expanded: true,
            });
            for category in Category::ORDER {
                let items: Vec<&&SchemaObject> = matches
                    .iter()
                    .filter(|o| Category::of(o.kind) == category)
                    .collect();
                if items.is_empty() {
                    continue;
                }
                rows.push(Row::Category {
                    schema: schema.clone(),
                    category,
                    expanded: true,
                    count: items.len(),
                });
                for o in items {
                    if shown < MAX_SHOWN {
                        shown += 1;
                        rows.push(Row::Object {
                            schema: schema.clone(),
                            name: o.name.clone(),
                            kind: o.kind,
                        });
                    }
                }
            }
        }
        if rows.is_empty() {
            rows.push(Row::Message("No matches".into()));
        } else if total > shown {
            rows.push(Row::Message(format!(
                "…and {} more. Narrow the search.",
                total - shown
            )));
        }
        rows
    }

    pub fn toggle_extensions(&mut self) {
        self.extensions_open = !self.extensions_open;
    }

    pub fn start_loading_schemas(&mut self) {
        self.schemas = Some(Load::Loading);
    }

    pub fn set_schemas(&mut self, result: Result<Vec<String>, String>) {
        self.schemas = Some(match result {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e),
        });
    }

    pub fn set_objects(&mut self, schema: &str, result: Result<Vec<SchemaObject>, String>) {
        self.objects.insert(
            schema.to_string(),
            match result {
                Ok(v) => Load::Ready(v),
                Err(e) => Load::Failed(e),
            },
        );
    }

    /// Toggles a schema; returns true when its objects must be fetched.
    pub fn toggle(&mut self, schema: &str) -> bool {
        if !self.expanded.remove(schema) {
            self.expanded.insert(schema.to_string());
            if !matches!(self.objects.get(schema), Some(Load::Ready(_))) {
                self.objects.insert(schema.to_string(), Load::Loading);
                return true;
            }
        }
        false
    }

    /// Schemas (with their objects when expanded) followed by the extensions section.
    pub fn rows(&self) -> Vec<Row> {
        if let Some(f) = &self.filter {
            return self.filtered_rows(f);
        }
        let msg = |s: &str| vec![Row::Message(s.to_string())];
        let schemas = match &self.schemas {
            None => return Vec::new(),
            Some(Load::Loading) => return msg("Loading…"),
            Some(Load::Failed(e)) => return msg(&format!("Couldn't load schemas: {e}")),
            Some(Load::Ready(s)) => s,
        };
        let mut rows = Vec::new();
        for name in schemas {
            let expanded = self.expanded.contains(name);
            rows.push(Row::Schema {
                name: name.clone(),
                expanded,
            });
            if !expanded {
                continue;
            }
            match self.objects.get(name) {
                Some(Load::Ready(objs)) if objs.is_empty() => {
                    rows.push(Row::Message("Empty".into()))
                }
                Some(Load::Ready(objs)) => {
                    for category in Category::ORDER {
                        let items: Vec<&SchemaObject> = objs
                            .iter()
                            .filter(|o| Category::of(o.kind) == category)
                            .collect();
                        if items.is_empty() {
                            continue;
                        }
                        let open = self.category_open(name, category);
                        rows.push(Row::Category {
                            schema: name.clone(),
                            category,
                            expanded: open,
                            count: items.len(),
                        });
                        if open {
                            rows.extend(items.into_iter().map(|o| Row::Object {
                                schema: name.clone(),
                                name: o.name.clone(),
                                kind: o.kind,
                            }));
                        }
                    }
                }
                Some(Load::Failed(e)) => rows.push(Row::Message(format!("Couldn't load: {e}"))),
                _ => rows.push(Row::Message("Loading…".into())),
            }
        }
        match &self.extensions {
            Some(Load::Ready(exts)) => {
                rows.push(Row::ExtensionsHeader {
                    expanded: self.extensions_open,
                    count: Some(exts.len()),
                });
                if self.extensions_open {
                    rows.extend(exts.iter().map(|(n, v)| Row::Extension {
                        name: n.clone(),
                        version: v.clone(),
                    }));
                }
            }
            Some(Load::Failed(e)) => {
                rows.push(Row::Message(format!("Couldn't load extensions: {e}")))
            }
            _ => {}
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(name: &str) -> SchemaObject {
        SchemaObject {
            name: name.into(),
            kind: ObjectKind::Table,
        }
    }

    #[test]
    fn lifecycle() {
        let mut t = SchemaTree::default();
        assert!(t.rows().is_empty());
        t.start_loading_schemas();
        assert_eq!(t.rows(), [Row::Message("Loading…".into())]);
        t.set_schemas(Err("boom".into()));
        assert_eq!(
            t.rows(),
            [Row::Message("Couldn't load schemas: boom".into())]
        );
        t.set_schemas(Ok(vec!["public".into(), "audit".into()]));
        assert_eq!(t.rows().len(), 2);

        assert!(t.toggle("public"));
        assert_eq!(t.rows()[1], Row::Message("Loading…".into()));
        t.set_objects("public", Ok(vec![obj("a"), obj("b")]));
        assert_eq!(t.rows().len(), 5, "schema, Tables group, two tables, audit");
        assert!(matches!(
            &t.rows()[1],
            Row::Category {
                category: Category::Tables,
                count: 2,
                expanded: true,
                ..
            }
        ));
        assert!(matches!(&t.rows()[2], Row::Object { name, .. } if name == "a"));

        assert!(!t.toggle("public"));
        assert_eq!(t.rows().len(), 2);
        assert!(!t.toggle("public"), "cached objects are not refetched");

        assert!(t.toggle("audit"));
        t.set_objects("audit", Ok(vec![]));
        assert!(t.rows().contains(&Row::Message("Empty".into())));
        t.toggle("audit");
        assert!(!t.toggle("audit"));
        t.set_objects("audit", Err("x".into()));
        assert!(t.rows().contains(&Row::Message("Couldn't load: x".into())));
        t = SchemaTree::default();
        assert!(t.rows().is_empty());
    }

    #[test]
    fn categories_group_and_toggle() {
        let mut t = SchemaTree::default();
        t.set_schemas(Ok(vec!["s".into()]));
        t.toggle("s");
        let o = |name: &str, kind| SchemaObject {
            name: name.into(),
            kind,
        };
        t.set_objects(
            "s",
            Ok(vec![
                o("t", ObjectKind::Table),
                o("v", ObjectKind::View),
                o("m", ObjectKind::MaterializedView),
                o("q", ObjectKind::Sequence),
                o("e", ObjectKind::EnumType),
                o("f(a integer)", ObjectKind::Function),
                o("p()", ObjectKind::Procedure),
            ]),
        );
        let rows = t.rows();
        let cats: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Category {
                    category,
                    count,
                    expanded,
                    ..
                } => Some((*category, *count, *expanded)),
                _ => None,
            })
            .collect();
        assert_eq!(
            cats,
            [
                (Category::Tables, 1, true),
                (Category::Views, 2, false),
                (Category::Sequences, 1, false),
                (Category::Types, 1, false),
                (Category::Functions, 2, false),
            ]
        );
        assert_eq!(rows.len(), 1 + 5 + 1, "only Tables lists its object");
        t.toggle_category("s", Category::Views);
        t.toggle_category("s", Category::Tables);
        let rows = t.rows();
        assert_eq!(rows.len(), 1 + 5 + 2, "Views open, Tables closed");
        assert!(
            rows.iter()
                .any(|r| matches!(r, Row::Object { name, .. } if name == "m"))
        );
        // Groups keep their state across a refresh.
        t.refresh();
        t.set_schemas(Ok(vec!["s".into()]));
        t.set_objects("s", Ok(vec![o("v", ObjectKind::View)]));
        assert!(matches!(
            &t.rows()[1],
            Row::Category {
                category: Category::Views,
                expanded: true,
                ..
            }
        ));
        for c in Category::ORDER {
            assert!(!c.label().is_empty());
        }
    }

    fn all_objects() -> Vec<(String, SchemaObject)> {
        let o = |s: &str, n: &str, kind| {
            (
                s.to_string(),
                SchemaObject {
                    name: n.into(),
                    kind,
                },
            )
        };
        vec![
            o("audit", "log", ObjectKind::Table),
            o("public", "orders", ObjectKind::Table),
            o("public", "order_items", ObjectKind::Table),
            o("public", "v_orders", ObjectKind::View),
            o("public", "customers", ObjectKind::Table),
            o("public", "f(a integer)", ObjectKind::Function),
        ]
    }

    #[test]
    fn search_filters_across_schemas() {
        let mut t = SchemaTree::default();
        t.set_schemas(Ok(vec!["audit".into(), "public".into()]));
        assert!(!t.needs_all_objects());
        t.set_filter("  ORDER ");
        assert!(t.needs_all_objects(), "a search needs the full object list");
        t.start_loading_all();
        assert!(!t.needs_all_objects());
        assert_eq!(t.rows(), [Row::Message("Searching…".into())]);
        t.set_all_objects(Ok(all_objects()));
        let rows = t.rows();
        let names: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Object { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            ["orders", "order_items", "v_orders"],
            "server order is kept"
        );
        assert!(matches!(&rows[0], Row::Schema { name, expanded: true } if name == "public"));
        assert!(rows.iter().any(|r| matches!(
            r,
            Row::Category {
                category: Category::Views,
                count: 1,
                expanded: true,
                ..
            }
        )));
        // A schema name match shows all of that schema's objects.
        t.set_filter("audit");
        assert!(matches!(&t.rows()[2], Row::Object { name, .. } if name == "log"));
        t.set_filter("zzz");
        assert_eq!(t.rows(), [Row::Message("No matches".into())]);
        t.set_all_objects(Err("boom".into()));
        assert_eq!(t.rows(), [Row::Message("Couldn't search: boom".into())]);
        // Clearing the search returns to the normal tree and keeps the list for next time.
        t.set_filter("   ");
        assert!(matches!(
            &t.rows()[0],
            Row::Schema {
                expanded: false,
                ..
            }
        ));
        t.set_filter("o");
        assert!(!t.needs_all_objects());
        // A refresh keeps the search but drops the cached list.
        t.refresh();
        assert!(t.needs_all_objects());
    }

    #[test]
    fn search_results_are_capped() {
        let mut t = SchemaTree::default();
        t.set_filter("t");
        let many: Vec<(String, SchemaObject)> = (0..1200)
            .map(|i| {
                (
                    "s".to_string(),
                    SchemaObject {
                        name: format!("t{i:04}"),
                        kind: ObjectKind::Table,
                    },
                )
            })
            .collect();
        t.set_all_objects(Ok(many));
        let rows = t.rows();
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, Row::Object { .. }))
                .count(),
            1000
        );
        assert_eq!(
            rows.last(),
            Some(&Row::Message("…and 200 more. Narrow the search.".into()))
        );
    }

    #[test]
    fn extensions_and_refresh() {
        let mut t = SchemaTree::default();
        t.set_schemas(Ok(vec!["public".into()]));
        t.set_extensions(Ok(vec![("plpgsql".into(), "1.0".into())]));
        let rows = t.rows();
        assert_eq!(
            rows[1],
            Row::ExtensionsHeader {
                expanded: false,
                count: Some(1)
            }
        );
        t.toggle_extensions();
        assert_eq!(t.rows().len(), 3);
        assert!(
            matches!(&t.rows()[2], Row::Extension { name, version } if name == "plpgsql" && version == "1.0")
        );
        t.toggle("public");
        t.set_objects("public", Ok(vec![obj("a")]));
        let again = t.refresh();
        assert_eq!(again, ["public"]);
        assert_eq!(t.rows(), Vec::<Row>::new(), "schemas must be reloaded");
        t.set_schemas(Ok(vec!["public".into()]));
        assert_eq!(
            t.rows()[1],
            Row::Message("Loading…".into()),
            "expansion is kept"
        );
        t.set_extensions(Err("x".into()));
        assert!(
            t.rows()
                .iter()
                .any(|r| matches!(r, Row::Message(m) if m.contains("extensions")))
        );
    }
}
