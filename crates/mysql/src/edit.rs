//! Build the SQL statements for staged grid edits. `preview()` and `apply()`
//! both use `build`, so the preview is exactly what runs.

use std::collections::HashSet;

use anyhow::bail;
use ourdb_core::{
    Change, Edit, Kind, ObjectRef, Value,
    sql::{mysql_literal, quote_ident, x_literal},
};

/// One statement and its row rule.
#[derive(Debug, PartialEq)]
pub struct Stmt {
    pub sql: String,
    /// UPDATE and DELETE must affect exactly one row.
    pub one_row: bool,
}

/// Settings that change how literals look.
pub struct Literals<'a> {
    /// False when `sql_mode` has NO_BACKSLASH_ESCAPES.
    pub backslash_escapes: bool,
    /// Binary columns of the table (BLOB, BINARY, VARBINARY, BIT, GEOMETRY).
    pub binary: &'a HashSet<String>,
}

/// `ns`.`name`, or only `name` when the namespace is empty.
pub fn table_name(obj: &ObjectRef) -> String {
    if obj.namespace.is_empty() {
        q(&obj.name)
    } else {
        format!("{}.{}", q(&obj.namespace), q(&obj.name))
    }
}

/// Quote a MySQL identifier.
pub fn q(name: &str) -> String {
    quote_ident(Kind::MySql, name)
}

/// The statements for `changes`, in order.
pub fn build(obj: &ObjectRef, changes: &[Change], lit: &Literals) -> anyhow::Result<Vec<Stmt>> {
    let table = table_name(obj);
    changes
        .iter()
        .map(|change| build_one(&table, change, lit))
        .collect()
}

fn build_one(table: &str, change: &Change, lit: &Literals) -> anyhow::Result<Stmt> {
    match change {
        Change::Update { key, set } => {
            if set.is_empty() {
                bail!("An update has no columns to change.");
            }
            let mut parts = Vec::with_capacity(set.len());
            for (col, original, edit) in set {
                let binary = lit.binary.contains(col) || matches!(original, Value::Bytes(_));
                parts.push(format!("{} = {}", q(col), lit.edit(edit, binary)?));
            }
            let sql = format!(
                "UPDATE {table} SET {} WHERE {}",
                parts.join(", "),
                lit.where_key(key)?
            );
            Ok(Stmt { sql, one_row: true })
        }
        Change::Insert { values } => {
            if values.is_empty() {
                return Ok(Stmt {
                    sql: format!("INSERT INTO {table} () VALUES ()"),
                    one_row: false,
                });
            }
            let mut cols = Vec::with_capacity(values.len());
            let mut vals = Vec::with_capacity(values.len());
            for (col, edit) in values {
                cols.push(q(col));
                vals.push(lit.edit(edit, lit.binary.contains(col))?);
            }
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                cols.join(", "),
                vals.join(", ")
            );
            Ok(Stmt {
                sql,
                one_row: false,
            })
        }
        Change::Delete { key } => {
            let sql = format!("DELETE FROM {table} WHERE {}", lit.where_key(key)?);
            Ok(Stmt { sql, one_row: true })
        }
        Change::InsertDoc { .. } | Change::ReplaceDoc { .. } => {
            bail!("Document changes are not supported on a MySQL connection.")
        }
    }
}

impl Literals<'_> {
    fn text(&self, s: &str) -> String {
        mysql_literal(s, self.backslash_escapes)
    }

    /// Literal for user input. Binary columns take `0x..` as hex.
    fn edit(&self, edit: &Edit, binary: bool) -> anyhow::Result<String> {
        match edit {
            Edit::Null => Ok("NULL".into()),
            Edit::Text(s) if binary && (s.starts_with("0x") || s.starts_with("0X")) => {
                hex_literal(&s[2..])
            }
            Edit::Text(s) => Ok(self.text(s)),
        }
    }

    /// `a = 1 AND b = 'x'` for the key columns of one row.
    fn where_key(&self, key: &[(String, Value)]) -> anyhow::Result<String> {
        if key.is_empty() {
            bail!("The row has no key, so it cannot be changed.");
        }
        let mut parts = Vec::with_capacity(key.len());
        for (col, value) in key {
            let cond = match value {
                Value::Null => format!("{} IS NULL", q(col)),
                Value::Text(s) => format!("{} = {}", q(col), self.text(s)),
                Value::Bytes(b) => format!("{} = {}", q(col), x_literal(b)),
                Value::Int(i) => format!("{} = {i}", q(col)),
                Value::Float(f) => format!("{} = {f}", q(col)),
                Value::Bool(b) => format!("{} = {}", q(col), u8::from(*b)),
                Value::Missing | Value::Json(_) => {
                    bail!("The key column {col} has no usable value.")
                }
            };
            parts.push(cond);
        }
        Ok(parts.join(" AND "))
    }
}

/// `X'..'` from the hex digits after `0x`.
fn hex_literal(digits: &str) -> anyhow::Result<String> {
    if !digits.len().is_multiple_of(2) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("'0x{digits}' is not valid hex. Use an even number of hex digits after 0x.");
    }
    Ok(format!("X'{digits}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Edit {
        Edit::Text(s.into())
    }

    fn run(changes: &[Change], backslash_escapes: bool) -> anyhow::Result<Vec<String>> {
        let binary: HashSet<String> = ["data".to_string()].into();
        let lit = Literals {
            backslash_escapes,
            binary: &binary,
        };
        let obj = ObjectRef::new("db", "t`x");
        Ok(build(&obj, changes, &lit)?
            .into_iter()
            .map(|s| s.sql)
            .collect())
    }

    #[test]
    fn update_insert_delete() {
        let key = vec![("id".to_string(), Value::Text("7".into()))];
        let changes = [
            Change::Update {
                key: key.clone(),
                set: vec![
                    (
                        "name".into(),
                        Value::Text("a".into()),
                        text("O'Brien \\ \n"),
                    ),
                    ("note".into(), Value::Null, Edit::Null),
                    ("data".into(), Value::Null, text("0xCAFE")),
                    ("raw".into(), Value::Bytes(vec![1]), text("0x")),
                    ("label".into(), Value::Text("x".into()), text("0x41")),
                ],
            },
            Change::Insert { values: vec![] },
            Change::Insert {
                values: vec![("name".into(), text("😀")), ("data".into(), text("0x00ff"))],
            },
            Change::Delete { key },
        ];
        let sql = run(&changes, true).unwrap();
        assert_eq!(
            sql,
            vec![
                r"UPDATE `db`.`t``x` SET `name` = 'O\'Brien \\ \n', `note` = NULL, `data` = X'CAFE', `raw` = X'', `label` = '0x41' WHERE `id` = '7'",
                "INSERT INTO `db`.`t``x` () VALUES ()",
                "INSERT INTO `db`.`t``x` (`name`, `data`) VALUES ('😀', X'00ff')",
                "DELETE FROM `db`.`t``x` WHERE `id` = '7'",
            ]
        );
    }

    #[test]
    fn no_backslash_escapes() {
        let changes = [Change::Insert {
            values: vec![("name".into(), text("O'Brien \\"))],
        }];
        assert_eq!(
            run(&changes, false).unwrap(),
            vec![r"INSERT INTO `db`.`t``x` (`name`) VALUES ('O''Brien \')"]
        );
    }

    #[test]
    fn key_forms_and_errors() {
        let key = vec![
            ("a".to_string(), Value::Bytes(vec![0xab])),
            ("b".to_string(), Value::Null),
            ("c".to_string(), Value::Int(3)),
        ];
        let sql = run(&[Change::Delete { key }], true).unwrap();
        assert_eq!(
            sql,
            vec!["DELETE FROM `db`.`t``x` WHERE `a` = X'ab' AND `b` IS NULL AND `c` = 3"]
        );
        assert!(run(&[Change::Delete { key: vec![] }], true).is_err());
        let bad_hex = Change::Insert {
            values: vec![("data".into(), text("0xabc"))],
        };
        assert!(run(&[bad_hex], true).is_err());
        assert!(run(&[Change::InsertDoc { json: "{}".into() }], true).is_err());
    }
}
