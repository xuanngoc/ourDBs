//! Export a result set as CSV, TSV (clipboard) or JSON.

use serde_json::{Map, Value as J};

use crate::{model::ResultSet, value::Value};

fn cell_text(v: &Value) -> String {
    match v {
        Value::Null | Value::Missing => String::new(),
        Value::Json(_) => serde_json::to_string(&v.to_json()).unwrap_or_default(),
        _ => v.edit_text(),
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// RFC 4180 CSV with a header row. NULL is an empty field.
pub fn to_csv(rs: &ResultSet) -> String {
    let mut out = String::new();
    out.push_str(
        &rs.columns
            .iter()
            .map(|c| csv_field(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");
    for row in &rs.rows {
        out.push_str(
            &row.iter()
                .map(|v| csv_field(&cell_text(v)))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    }
    out
}

/// Tab-separated rows for the clipboard (tabs and line breaks become spaces).
pub fn to_tsv(columns: &[String], rows: &[&[Value]], header: bool) -> String {
    let clean = |s: String| s.replace(['\t', '\n', '\r'], " ");
    let mut lines = Vec::new();
    if header {
        lines.push(columns.join("\t"));
    }
    for row in rows {
        lines.push(
            row.iter()
                .map(|v| clean(cell_text(v)))
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    lines.join("\n")
}

/// One JSON object per row.
pub fn row_json(columns: &[String], row: &[Value]) -> J {
    let mut m = Map::new();
    for (c, v) in columns.iter().zip(row) {
        if !v.is_missing() {
            m.insert(c.clone(), v.to_json());
        }
    }
    J::Object(m)
}

/// Pretty JSON array of row objects.
pub fn to_json(rs: &ResultSet) -> String {
    let rows: Vec<J> = rs.rows.iter().map(|r| row_json(&rs.columns, r)).collect();
    serde_json::to_string_pretty(&rows).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv() {
        let rs = ResultSet {
            columns: vec!["a".into(), "b".into()],
            types: vec![String::new(), String::new()],
            rows: vec![vec![Value::Text("x,\"y\"".into()), Value::Null]],
            ..Default::default()
        };
        assert_eq!(to_csv(&rs), "a,b\r\n\"x,\"\"y\"\"\",\r\n");
        assert_eq!(
            to_json(&rs),
            "[\n  {\n    \"a\": \"x,\\\"y\\\"\",\n    \"b\": null\n  }\n]"
        );
    }
}
