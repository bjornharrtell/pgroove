//! EXPLAIN plan parsing and execution.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio_postgres::{Client, SimpleQueryMessage};

#[derive(Debug, Clone, PartialEq)]
pub struct PlanNode {
    pub node_type: String,
    pub relation: Option<String>,
    pub alias: Option<String>,
    pub index_name: Option<String>,
    pub filter: Option<String>,
    pub total_cost: f64,
    pub plan_rows: f64,
    pub actual_rows: Option<f64>,
    /// Per-loop total time, as reported.
    pub actual_total_ms: Option<f64>,
    pub loops: f64,
    pub shared_hit: Option<i64>,
    pub shared_read: Option<i64>,
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    fn total_time(&self) -> Option<f64> {
        Some(self.actual_total_ms? * self.loops.max(1.))
    }

    /// Time spent in this node excluding its children.
    pub fn exclusive_ms(&self) -> Option<f64> {
        let own = self.total_time()?;
        let kids: f64 = self.children.iter().filter_map(PlanNode::total_time).sum();
        Some((own - kids).max(0.))
    }

    /// Estimated cost of this node excluding its children.
    pub fn exclusive_cost(&self) -> f64 {
        let kids: f64 = self.children.iter().map(|c| c.total_cost).sum();
        (self.total_cost - kids).max(0.)
    }

    /// One-line description such as `Seq Scan on orders o`.
    pub fn title(&self) -> String {
        let mut s = self.node_type.clone();
        if let Some(i) = &self.index_name {
            s.push_str(&format!(" using {i}"));
        }
        if let Some(r) = &self.relation {
            s.push_str(&format!(" on {r}"));
            if let Some(a) = self.alias.as_ref().filter(|a| *a != r) {
                s.push_str(&format!(" {a}"));
            }
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub root: PlanNode,
    pub planning_ms: Option<f64>,
    pub execution_ms: Option<f64>,
    pub analyzed: bool,
}

/// A node placed in display order.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatNode<'a> {
    pub depth: usize,
    pub node: &'a PlanNode,
    /// Fraction of the total (time when analyzed, otherwise cost) spent in this node alone.
    pub share: f64,
    pub hot: bool,
}

fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k)?.as_f64()
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(str::to_string)
}

fn node(v: &Value) -> Result<PlanNode> {
    Ok(PlanNode {
        node_type: s(v, "Node Type").ok_or_else(|| anyhow!("plan node without a type"))?,
        relation: s(v, "Relation Name"),
        alias: s(v, "Alias"),
        index_name: s(v, "Index Name"),
        filter: s(v, "Filter"),
        total_cost: f(v, "Total Cost").unwrap_or(0.),
        plan_rows: f(v, "Plan Rows").unwrap_or(0.),
        actual_rows: f(v, "Actual Rows"),
        actual_total_ms: f(v, "Actual Total Time"),
        loops: f(v, "Actual Loops").unwrap_or(1.),
        shared_hit: v.get("Shared Hit Blocks").and_then(Value::as_i64),
        shared_read: v.get("Shared Read Blocks").and_then(Value::as_i64),
        children: v
            .get("Plans")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(node).collect::<Result<Vec<_>>>())
            .transpose()?
            .unwrap_or_default(),
    })
}

/// Parses the output of `EXPLAIN (FORMAT JSON)`.
pub fn parse(json: &str) -> Result<Plan> {
    let v: Value = serde_json::from_str(json).context("plan is not valid JSON")?;
    let top = v.get(0).ok_or_else(|| anyhow!("empty plan"))?;
    let root = node(top.get("Plan").ok_or_else(|| anyhow!("plan has no root"))?)?;
    Ok(Plan {
        analyzed: root.actual_total_ms.is_some(),
        root,
        planning_ms: f(top, "Planning Time"),
        execution_ms: f(top, "Execution Time"),
    })
}

impl Plan {
    /// Nodes in display order with their cost share and the hottest one marked.
    pub fn flatten(&self) -> Vec<FlatNode<'_>> {
        let total = if self.analyzed {
            self.root.total_time().unwrap_or(0.)
        } else {
            self.root.total_cost
        };
        let mut out = Vec::new();
        fn walk<'a>(
            n: &'a PlanNode,
            depth: usize,
            total: f64,
            analyzed: bool,
            out: &mut Vec<FlatNode<'a>>,
        ) {
            let own = if analyzed {
                n.exclusive_ms().unwrap_or(0.)
            } else {
                n.exclusive_cost()
            };
            out.push(FlatNode {
                depth,
                node: n,
                share: if total > 0. { own / total } else { 0. },
                hot: false,
            });
            for c in &n.children {
                walk(c, depth + 1, total, analyzed, out);
            }
        }
        walk(&self.root, 0, total, self.analyzed, &mut out);
        if let Some(i) = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.share.total_cmp(&b.1.share))
            .map(|(i, _)| i)
        {
            out[i].hot = out[i].share > 0.;
        }
        out
    }
}

/// Explains one statement. With `analyze` the statement is executed inside a
/// transaction that is always rolled back.
pub async fn run(client: &Client, sql: &str, analyze: bool) -> Result<Plan> {
    let sql = sql.trim().trim_end_matches(';').trim();
    if sql.is_empty() {
        bail!("nothing to explain");
    }
    let prefix = if analyze {
        "explain (analyze, buffers, format json) "
    } else {
        "explain (format json) "
    };
    if analyze {
        client.batch_execute("begin").await?;
    }
    let result = client.simple_query(&format!("{prefix}{sql}")).await;
    if analyze {
        let _ = client.batch_execute("rollback").await;
    }
    let msgs = result.map_err(|e| anyhow!(crate::query::describe_error(&e)))?;
    let text = msgs
        .iter()
        .find_map(|m| match m {
            SimpleQueryMessage::Row(r) => r.get(0).map(str::to_string),
            _ => None,
        })
        .ok_or_else(|| anyhow!("the server returned no plan"))?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[{"Plan":{"Node Type":"Hash Join","Total Cost":100.0,"Plan Rows":10,
      "Actual Rows":10,"Actual Total Time":50.0,"Actual Loops":1,
      "Plans":[
        {"Node Type":"Seq Scan","Relation Name":"orders","Alias":"o","Total Cost":60.0,
         "Plan Rows":100,"Actual Rows":100,"Actual Total Time":40.0,"Actual Loops":1,
         "Filter":"(total > 5)","Shared Hit Blocks":10,"Shared Read Blocks":2},
        {"Node Type":"Index Scan","Index Name":"c_pkey","Relation Name":"customers","Alias":"customers",
         "Total Cost":20.0,"Plan Rows":1,"Actual Rows":1,"Actual Total Time":2.0,"Actual Loops":3}
      ]},"Planning Time":0.5,"Execution Time":51.0}]"#;

    #[test]
    fn parses_and_computes_exclusive_time() {
        let p = parse(SAMPLE).unwrap();
        assert!(p.analyzed);
        assert_eq!((p.planning_ms, p.execution_ms), (Some(0.5), Some(51.0)));
        assert_eq!(p.root.children.len(), 2);
        // 50 - (40 + 2*3) = 4
        assert_eq!(p.root.exclusive_ms(), Some(4.0));
        assert_eq!(p.root.exclusive_cost(), 20.0);
        assert_eq!(p.root.children[0].title(), "Seq Scan on orders o");
        assert_eq!(
            p.root.children[1].title(),
            "Index Scan using c_pkey on customers"
        );
        assert_eq!(p.root.children[0].shared_read, Some(2));
        assert_eq!(p.root.children[0].filter.as_deref(), Some("(total > 5)"));
    }

    #[test]
    fn flatten_marks_hottest_node() {
        let p = parse(SAMPLE).unwrap();
        let flat = p.flatten();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].depth, 0);
        assert_eq!(flat[1].depth, 1);
        assert!(flat[1].hot && !flat[0].hot && !flat[2].hot);
        assert!((flat[1].share - 0.8).abs() < 1e-9);
    }

    #[test]
    fn plain_plan_uses_cost() {
        let p = parse(r#"[{"Plan":{"Node Type":"Seq Scan","Relation Name":"t","Alias":"t","Total Cost":10.0,"Plan Rows":5}}]"#)
            .unwrap();
        assert!(!p.analyzed);
        let flat = p.flatten();
        assert!(flat[0].hot);
        assert_eq!(flat[0].share, 1.0);
        assert_eq!(flat[0].node.title(), "Seq Scan on t");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("nope").is_err());
        assert!(parse("[]").is_err());
        assert!(parse(r#"[{"x":1}]"#).is_err());
        assert!(parse(r#"[{"Plan":{}}]"#).is_err());
    }

    #[tokio::test]
    async fn explain_live() {
        let Some((client, _)) = crate::connect::test_connection().await else {
            return;
        };
        let p = run(&client, "select 1;", false).await.unwrap();
        assert!(!p.analyzed);
        client
            .batch_execute("drop table if exists pgroove_x; create table pgroove_x (id int)")
            .await
            .unwrap();
        let p = run(&client, "insert into pgroove_x values (1)", true)
            .await
            .unwrap();
        assert!(p.analyzed);
        let n: i64 = client
            .query_one("select count(*) from pgroove_x", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 0, "analyze runs are rolled back");
        assert!(run(&client, "  ; ", false).await.is_err());
        assert!(
            run(&client, "select * from missing_table", false)
                .await
                .is_err()
        );
        client.batch_execute("drop table pgroove_x").await.unwrap();
    }
}
