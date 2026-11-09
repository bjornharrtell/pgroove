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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Schema {
        name: String,
        expanded: bool,
    },
    Object {
        schema: String,
        name: String,
        kind: ObjectKind,
    },
    Message(String),
}

#[derive(Default)]
pub struct SchemaTree {
    schemas: Option<Load<Vec<String>>>,
    objects: HashMap<String, Load<Vec<SchemaObject>>>,
    expanded: HashSet<String>,
}

impl SchemaTree {
    pub fn reset(&mut self) {
        *self = Self::default();
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

    pub fn rows(&self) -> Vec<Row> {
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
                Some(Load::Ready(objs)) => rows.extend(objs.iter().map(|o| Row::Object {
                    schema: name.clone(),
                    name: o.name.clone(),
                    kind: o.kind,
                })),
                Some(Load::Failed(e)) => rows.push(Row::Message(format!("Couldn't load: {e}"))),
                _ => rows.push(Row::Message("Loading…".into())),
            }
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
        assert_eq!(t.rows().len(), 4);
        assert!(matches!(&t.rows()[1], Row::Object { name, .. } if name == "a"));

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
        t.reset();
        assert!(t.rows().is_empty());
    }
}
