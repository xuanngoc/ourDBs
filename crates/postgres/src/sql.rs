//! SQL text that the driver builds: browse queries and edit statements.

use std::collections::HashMap;

use anyhow::bail;
use ourdb_core::{
    BrowseRequest, Change, Edit, Kind, Value,
    sql::{pg_bytea_literal, pg_literal, qualified, quote_ident, split_statements},
};

fn ident(name: &str) -> String {
    quote_ident(Kind::Postgres, name)
}

/// `WHERE (<filter>)` with a leading space, or "" for an empty filter.
/// The filter starts on a new line, so an error position maps to the filter
/// (see `filter_error` in lib.rs).
pub fn where_clause(filter: &str) -> anyhow::Result<String> {
    let f = filter.trim().trim_end_matches(';').trim_end();
    if f.is_empty() {
        return Ok(String::new());
    }
    if split_statements(f).len() > 1 {
        bail!("The filter must be one expression without ';'.");
    }
    // The new line after the filter ends a `--` comment in it.
    Ok(format!(" WHERE (\n{f}\n)"))
}

/// The page query. `pk` is the primary key in index order.
pub fn browse_sql(req: &BrowseRequest, pk: &[String]) -> anyhow::Result<String> {
    let mut sql = format!(
        "SELECT * FROM {}{}",
        qualified(Kind::Postgres, &req.object.namespace, &req.object.name),
        where_clause(&req.filter)?
    );
    let mut order: Vec<String> = Vec::new();
    if let Some((col, asc)) = &req.sort {
        order.push(format!(
            "{} {}",
            ident(col),
            if *asc { "ASC" } else { "DESC" }
        ));
    }
    // The key columns make the order unique, so pages are stable.
    for k in pk {
        if req.sort.as_ref().is_none_or(|(col, _)| col != k) {
            order.push(ident(k));
        }
    }
    if !order.is_empty() {
        sql.push_str(" ORDER BY ");
        sql.push_str(&order.join(", "));
    }
    sql.push_str(&format!(" LIMIT {} OFFSET {}", req.limit, req.offset));
    Ok(sql)
}

pub fn count_sql(req: &BrowseRequest) -> anyhow::Result<String> {
    Ok(format!(
        "SELECT count(*) FROM {}{}",
        qualified(Kind::Postgres, &req.object.namespace, &req.object.name),
        where_clause(&req.filter)?
    ))
}

/// One statement for one change. `table` is the quoted table name, `types`
/// maps column names to their catalog type.
pub fn change_sql(
    table: &str,
    change: &Change,
    types: &HashMap<String, String>,
    std_strings: bool,
) -> anyhow::Result<String> {
    match change {
        Change::Update { key, set } => {
            if set.is_empty() {
                bail!("no columns to update");
            }
            let sets: Vec<String> = set
                .iter()
                .map(|(col, _, edit)| {
                    format!(
                        "{} = {}",
                        ident(col),
                        edit_literal(col, edit, types, std_strings)
                    )
                })
                .collect();
            Ok(format!(
                "UPDATE {table} SET {} WHERE {}",
                sets.join(", "),
                key_match(key, std_strings)?
            ))
        }
        Change::Insert { values } => {
            if values.is_empty() {
                return Ok(format!("INSERT INTO {table} DEFAULT VALUES"));
            }
            let cols: Vec<String> = values.iter().map(|(col, _)| ident(col)).collect();
            let vals: Vec<String> = values
                .iter()
                .map(|(col, edit)| edit_literal(col, edit, types, std_strings))
                .collect();
            Ok(format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                cols.join(", "),
                vals.join(", ")
            ))
        }
        Change::Delete { key } => Ok(format!(
            "DELETE FROM {table} WHERE {}",
            key_match(key, std_strings)?
        )),
        Change::InsertDoc { .. } | Change::ReplaceDoc { .. } => {
            bail!("document changes do not work on PostgreSQL")
        }
    }
}

/// `"a" = '1' AND "b" = '2'` for the key of one row.
fn key_match(key: &[(String, Value)], std_strings: bool) -> anyhow::Result<String> {
    if key.is_empty() {
        bail!("the row has no key");
    }
    let parts: Vec<String> = key
        .iter()
        .map(|(col, v)| match v {
            Value::Null | Value::Missing => format!("{} IS NULL", ident(col)),
            Value::Bytes(b) => format!("{} = {}", ident(col), pg_bytea_literal(b, std_strings)),
            other => format!(
                "{} = {}",
                ident(col),
                pg_literal(&other.edit_text(), std_strings)
            ),
        })
        .collect();
    Ok(parts.join(" AND "))
}

/// A quoted literal; the server converts it to the column type. For a bytea
/// column, input that starts with `0x` becomes a hex bytea literal.
fn edit_literal(
    col: &str,
    edit: &Edit,
    types: &HashMap<String, String>,
    std_strings: bool,
) -> String {
    match edit {
        Edit::Null => "NULL".into(),
        Edit::Text(s) => {
            if types.get(col).is_some_and(|t| t == "bytea")
                && let Some(bytes) = parse_0x(s)
            {
                return pg_bytea_literal(&bytes, std_strings);
            }
            pg_literal(s, std_strings)
        }
    }
}

/// Bytes of `0x0a1b...`, or `None` when the text is not that form.
fn parse_0x(s: &str) -> Option<Vec<u8>> {
    let hex = s
        .trim()
        .strip_prefix("0x")
        .or_else(|| s.trim().strip_prefix("0X"))?;
    if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Query editor helpers
// ---------------------------------------------------------------------------

/// The statement text after leading spaces and comments.
pub fn skip_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, r)| r);
        } else {
            return s;
        }
    }
}

/// First keyword of a statement in upper case, for example "UPDATE".
pub fn first_word(stmt: &str) -> String {
    skip_comments(stmt)
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// psql-like status text for a statement without rows: "UPDATE 3", "CREATE".
pub fn status_text(word: &str, n: u64) -> String {
    match word {
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "SELECT" | "COPY" | "MOVE" | "FETCH" => {
            format!("{word} {n}")
        }
        "" => "OK".into(),
        w => w.to_string(),
    }
}

/// COPY with STDIN or STDOUT needs the copy sub-protocol, which the simple
/// query stream does not handle. COPY FROM STDIN would block the session.
pub fn is_copy_stdio(stmt: &str) -> bool {
    if first_word(stmt) != "COPY" {
        return false;
    }
    let upper = stmt.to_ascii_uppercase();
    upper.contains("STDIN") || upper.contains("STDOUT")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ourdb_core::ObjectRef;

    fn req(filter: &str, sort: Option<(&str, bool)>) -> BrowseRequest {
        BrowseRequest {
            object: ObjectRef::new("s", "t"),
            filter: filter.into(),
            sort: sort.map(|(c, a)| (c.to_string(), a)),
            limit: 50,
            offset: 100,
        }
    }

    #[test]
    fn browse_text() {
        let pk = vec!["id".to_string()];
        assert_eq!(
            browse_sql(&req("", None), &pk).unwrap(),
            r#"SELECT * FROM "s"."t" ORDER BY "id" LIMIT 50 OFFSET 100"#
        );
        assert_eq!(
            browse_sql(&req("a > 1 -- note;", Some(("a", false))), &pk).unwrap(),
            "SELECT * FROM \"s\".\"t\" WHERE (\na > 1 -- note\n) ORDER BY \"a\" DESC, \"id\" LIMIT 50 OFFSET 100"
        );
        assert_eq!(
            browse_sql(&req("", None), &[]).unwrap(),
            r#"SELECT * FROM "s"."t" LIMIT 50 OFFSET 100"#
        );
        assert!(browse_sql(&req("1=1; delete from t", None), &pk).is_err());
        assert_eq!(where_clause(" a = ';' ; ").unwrap(), " WHERE (\na = ';'\n)");
    }

    #[test]
    fn edit_text() {
        let types: HashMap<String, String> = [("b".to_string(), "bytea".to_string())].into();
        let up = Change::Update {
            key: vec![("id".into(), Value::Text("7".into()))],
            set: vec![
                ("name".into(), Value::Null, Edit::Text("O'Brien \\".into())),
                ("b".into(), Value::Null, Edit::Text("0xDEad".into())),
                ("n".into(), Value::Null, Edit::Null),
            ],
        };
        assert_eq!(
            change_sql("t", &up, &types, true).unwrap(),
            r#"UPDATE t SET "name" = 'O''Brien \', "b" = '\xdead'::bytea, "n" = NULL WHERE "id" = '7'"#
        );
        assert_eq!(
            change_sql("t", &up, &types, false).unwrap(),
            r#"UPDATE t SET "name" = E'O''Brien \\', "b" = E'\\xdead'::bytea, "n" = NULL WHERE "id" = E'7'"#
        );
        let ins = Change::Insert { values: vec![] };
        assert_eq!(
            change_sql("t", &ins, &types, true).unwrap(),
            "INSERT INTO t DEFAULT VALUES"
        );
        let del = Change::Delete {
            key: vec![("a".into(), Value::Int(1)), ("b".into(), Value::Null)],
        };
        assert_eq!(
            change_sql("t", &del, &types, true).unwrap(),
            r#"DELETE FROM t WHERE "a" = '1' AND "b" IS NULL"#
        );
        assert!(change_sql("t", &Change::Delete { key: vec![] }, &types, true).is_err());
        assert_eq!(parse_0x("0xaé1"), None);
        assert_eq!(parse_0x("0x"), Some(vec![]));
    }

    #[test]
    fn editor_helpers() {
        assert_eq!(first_word("-- hi\n/* x */ update t set a = 1"), "UPDATE");
        assert_eq!(skip_comments("-- only a comment"), "");
        assert_eq!(status_text("UPDATE", 3), "UPDATE 3");
        assert_eq!(status_text("CREATE", 0), "CREATE");
        assert!(is_copy_stdio("copy t from stdin"));
        assert!(!is_copy_stdio("select 'stdin'"));
    }
}
