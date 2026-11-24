//! Conversions: BSON to grid values, typed edits, and mongosh text.

use std::{collections::HashSet, fmt::Write as _};

use anyhow::{Result, anyhow, bail};
use bson::{Binary, Bson, DateTime, Decimal128, Document, spec::BinarySubtype};
use ourdb_core::{Edit, ResultSet, Value, value::iso_from_millis};
use serde_json::Value as J;

use crate::shell;

/// BSON to a grid value (SPEC section 4). Types without their own `Value`
/// variant become canonical Extended JSON.
pub fn to_value(b: &Bson) -> Value {
    match b {
        Bson::Null => Value::Null,
        Bson::Boolean(x) => Value::Bool(*x),
        Bson::Int32(i) => Value::Int(i64::from(*i)),
        Bson::Double(f) => Value::Float(*f),
        Bson::String(s) => Value::Text(s.clone()),
        other => Value::Json(other.clone().into_canonical_extjson()),
    }
}

/// A grid value back to BSON, for example the `_id` of a row.
pub fn from_value(v: &Value) -> Result<Bson> {
    Ok(match v {
        Value::Null => Bson::Null,
        Value::Missing => bail!("the value is missing"),
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Int(i) => int_bson(*i),
        Value::Float(f) => Bson::Double(*f),
        Value::Text(s) => Bson::String(s.clone()),
        Value::Bytes(b) => Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: b.clone(),
        }),
        Value::Json(j) => {
            Bson::try_from(j.clone()).map_err(|e| anyhow!("invalid Extended JSON {j}: {e}"))?
        }
    })
}

/// Int32 when the number fits, else Int64.
fn int_bson(n: i64) -> Bson {
    i32::try_from(n).map(Bson::Int32).unwrap_or(Bson::Int64(n))
}

/// The wrapper key of a scalar Extended JSON value (`$oid`, `$date`, ...).
fn wrapper_key(j: &J) -> Option<&str> {
    match j {
        J::Object(m) if m.len() == 1 => m
            .keys()
            .next()
            .map(String::as_str)
            .filter(|k| k.starts_with('$')),
        _ => None,
    }
}

/// Convert the input for one cell from the type of the original value
/// (SPEC section 6, typed edits).
pub fn typed_edit(original: &Value, edit: &Edit) -> Result<Bson> {
    let text = match edit {
        Edit::Null => return Ok(Bson::Null),
        Edit::Text(t) => t,
    };
    let t = text.trim();
    // The value of a shell helper such as NumberLong(5) or ObjectId("…").
    let shell = || shell::parse_value(t).ok();
    match original {
        Value::Text(_) => Ok(Bson::String(text.clone())),
        Value::Int(_) => integer(t, shell()).map(int_bson),
        Value::Float(_) => match (t.parse::<f64>(), shell()) {
            (Ok(f), _) => Ok(Bson::Double(f)),
            (_, Some(Bson::Double(f))) => Ok(Bson::Double(f)),
            (_, Some(Bson::Int32(i))) => Ok(Bson::Double(f64::from(i))),
            (_, Some(Bson::Int64(i))) => Ok(Bson::Double(i as f64)),
            _ => bail!("expected a number, got `{t}`"),
        },
        Value::Bool(_) => match t.to_ascii_lowercase().as_str() {
            "true" => Ok(Bson::Boolean(true)),
            "false" => Ok(Bson::Boolean(false)),
            _ => bail!("expected true or false, got `{t}`"),
        },
        Value::Null | Value::Missing | Value::Bytes(_) => {
            Ok(shell::parse_value(text).unwrap_or_else(|_| Bson::String(text.clone())))
        }
        Value::Json(j) => match wrapper_key(j) {
            Some("$numberLong") => integer(t, shell()).map(Bson::Int64),
            Some("$numberDecimal") => match (t.parse::<Decimal128>(), shell()) {
                (Ok(d), _) => Ok(Bson::Decimal128(d)),
                (_, Some(Bson::Decimal128(d))) => Ok(Bson::Decimal128(d)),
                _ => bail!("expected a decimal number, got `{t}`"),
            },
            Some("$oid") => match (shell::parse_object_id(t), shell()) {
                (Ok(oid), _) => Ok(Bson::ObjectId(oid)),
                (_, Some(Bson::ObjectId(oid))) => Ok(Bson::ObjectId(oid)),
                _ => bail!("expected 24 hex characters or ObjectId(\"…\"), got `{t}`"),
            },
            Some("$date") => {
                if let Ok(ms) = t.parse::<i64>() {
                    return Ok(Bson::DateTime(DateTime::from_millis(ms)));
                }
                match (shell::parse_date(t), shell()) {
                    (Ok(d), _) => Ok(Bson::DateTime(d)),
                    (_, Some(Bson::DateTime(d))) => Ok(Bson::DateTime(d)),
                    _ => bail!(
                        "expected an ISO date (2024-05-01T10:00:00Z), ISODate(\"…\") or epoch milliseconds, got `{t}`"
                    ),
                }
            }
            // Documents, arrays and the other wrappers ($binary, $timestamp, ...).
            _ => shell::parse_value(text),
        },
    }
}

fn integer(t: &str, shell: Option<Bson>) -> Result<i64> {
    match (t.parse::<i64>(), shell) {
        (Ok(n), _) => Ok(n),
        (_, Some(Bson::Int32(n))) => Ok(i64::from(n)),
        (_, Some(Bson::Int64(n))) => Ok(n),
        _ => bail!("expected an integer, got `{t}`"),
    }
}

// ---------------------------------------------------------------------------
// mongosh text
// ---------------------------------------------------------------------------

/// mongosh text for a BSON value: `ObjectId('…')`, `ISODate('…')`,
/// `NumberLong(5)`, unquoted keys. Our shell parser reads it back.
pub fn shell_text(b: &Bson) -> String {
    let mut out = String::new();
    write_bson(b, &mut out);
    out
}

/// mongosh text for a document.
pub fn shell_doc(d: &Document) -> String {
    let mut out = String::new();
    write_doc(d, &mut out);
    out
}

fn write_bson(b: &Bson, out: &mut String) {
    match b {
        Bson::Null => out.push_str("null"),
        Bson::Boolean(x) => out.push_str(if *x { "true" } else { "false" }),
        Bson::Int32(i) => out.push_str(&i.to_string()),
        Bson::Int64(i) => _ = write!(out, "NumberLong({i})"),
        Bson::Double(f) => out.push_str(&double_text(*f)),
        Bson::Decimal128(d) => _ = write!(out, "NumberDecimal('{d}')"),
        Bson::String(s) => out.push_str(&quote(s)),
        Bson::ObjectId(o) => _ = write!(out, "ObjectId('{}')", o.to_hex()),
        Bson::DateTime(d) => {
            _ = write!(out, "ISODate('{}')", iso_from_millis(d.timestamp_millis()))
        }
        Bson::Document(d) => write_doc(d, out),
        Bson::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_bson(v, out);
            }
            out.push(']');
        }
        // Binary, Timestamp, RegExp, MinKey, ...: Extended JSON in shell style.
        other => write_json(&other.clone().into_relaxed_extjson(), out),
    }
}

fn write_doc(d: &Document, out: &mut String) {
    if d.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push('{');
    for (i, (k, v)) in d.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&key_text(k));
        out.push_str(": ");
        write_bson(v, out);
    }
    out.push('}');
}

fn write_json(j: &J, out: &mut String) {
    match j {
        J::Object(m) => {
            out.push('{');
            for (i, (k, v)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&key_text(k));
                out.push_str(": ");
                write_json(v, out);
            }
            out.push('}');
        }
        J::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_json(v, out);
            }
            out.push(']');
        }
        J::String(s) => out.push_str(&quote(s)),
        other => out.push_str(&other.to_string()),
    }
}

/// A double that parses back as a double: `3.0`, `0.5`, `1e300`, `NaN`.
fn double_text(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    // serde_json prints the shortest form and keeps ".0" on whole numbers.
    serde_json::Number::from_f64(f)
        .map(|n| n.to_string())
        .unwrap_or_else(|| f.to_string())
}

/// A JavaScript string literal in single quotes.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => _ = write!(out, "\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn key_text(k: &str) -> String {
    if is_identifier(k) {
        k.to_string()
    } else {
        quote(k)
    }
}

/// `db.users`, or `db.getCollection('my-coll')` when the name needs quotes.
pub fn coll_text(name: &str) -> String {
    if name.split('.').all(is_identifier) {
        format!("db.{name}")
    } else {
        format!("db.getCollection({})", quote(name))
    }
}

// ---------------------------------------------------------------------------
// Grid results
// ---------------------------------------------------------------------------

/// Top-level field names in first-seen order, with `_id` first when present.
pub fn union_columns<'a>(docs: impl IntoIterator<Item = &'a Document>) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for d in docs {
        for k in d.keys() {
            if seen.insert(k.as_str()) {
                cols.push(k.clone());
            }
        }
    }
    if let Some(p) = cols.iter().position(|c| c == "_id") {
        let id = cols.remove(p);
        cols.insert(0, id);
    }
    cols
}

/// The most common type name of each column (first seen wins a tie).
/// An empty string when no document has the field.
pub fn column_types(docs: &[Document], columns: &[String]) -> Vec<String> {
    columns
        .iter()
        .map(|c| {
            let mut counts: Vec<(&'static str, usize)> = Vec::new();
            for d in docs {
                if let Some(v) = d.get(c) {
                    let t = shell::type_name(v);
                    match counts.iter_mut().find(|(n, _)| *n == t) {
                        Some((_, k)) => *k += 1,
                        None => counts.push((t, 1)),
                    }
                }
            }
            let mut best: Option<(&str, usize)> = None;
            for (t, k) in counts {
                if best.is_none_or(|(_, bk)| k > bk) {
                    best = Some((t, k));
                }
            }
            best.map(|(t, _)| t.to_string()).unwrap_or_default()
        })
        .collect()
}

/// Flatten documents into a grid: one column per top-level field.
/// A field that a document does not have is `Value::Missing`.
pub fn docs_result(docs: &[Document], columns: Vec<String>) -> ResultSet {
    let rows = docs
        .iter()
        .map(|d| {
            columns
                .iter()
                .map(|c| d.get(c).map(to_value).unwrap_or(Value::Missing))
                .collect()
        })
        .collect();
    let types = column_types(docs, &columns);
    ResultSet {
        columns,
        types,
        rows,
        ..ResultSet::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{Timestamp, doc, oid::ObjectId};
    use ourdb_core::value::row_document;

    fn sample() -> Document {
        doc! {
            "_id": ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap(),
            "name": "Ann",
            "when": DateTime::from_millis(1_700_000_000_123),
            "old": DateTime::from_millis(-86_400_000),
            "i32": 7,
            "i64": 3_000_000_000_i64,
            "small64": Bson::Int64(5),
            "dbl": 2.5,
            "whole": 3.0,
            "neg0": -0.0,
            "dec": Bson::Decimal128("12.50".parse().unwrap()),
            "nested": {"z": 1, "a": {"deep": [1, "x", 2.0]}, "n": Bson::Int64(9)},
            "arr": [1, "two", {"three": 3}],
            "bin": Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: vec![0, 1, 255] }),
            "ts": Bson::Timestamp(Timestamp { time: 1, increment: 2 }),
            "nil": Bson::Null,
            "yes": true,
        }
    }

    #[test]
    fn grid_values() {
        let d = sample();
        let v = |k: &str| to_value(d.get(k).unwrap());
        assert_eq!(v("name"), Value::Text("Ann".into()));
        assert_eq!(v("i32"), Value::Int(7));
        assert_eq!(v("dbl"), Value::Float(2.5));
        assert_eq!(v("nil"), Value::Null);
        assert_eq!(v("yes"), Value::Bool(true));
        assert_eq!(v("_id").display(), "ObjectId('64b7f0c2a1b2c3d4e5f60718')");
        assert_eq!(v("_id").edit_text(), "64b7f0c2a1b2c3d4e5f60718");
        assert_eq!(v("when").display(), "2023-11-14T22:13:20.123Z");
        assert_eq!(v("when").edit_text(), "2023-11-14T22:13:20.123Z");
        assert_eq!(v("old").edit_text(), "1969-12-31T00:00:00.000Z");
        assert_eq!(v("i64").display(), "3000000000");
        assert!(v("i64").is_number());
        assert_eq!(v("dec").display(), "12.50");
        assert_eq!(
            v("nested").display(),
            r#"{"z":1,"a":{"deep":[1,"x",2.0]},"n":{"$numberLong":"9"}}"#
        );
        assert_eq!(v("bin").display(), "Binary('AAH/', 00)");
        assert_eq!(v("ts").display(), "Timestamp(1, 2)");
    }

    /// row_document -> JSON text -> shell parse gives the same BSON, with
    /// the same field order.
    #[test]
    fn row_round_trip() {
        let d = sample();
        let columns: Vec<String> = d.keys().cloned().collect();
        let row: Vec<Value> = d.values().map(to_value).collect();
        let text = serde_json::to_string_pretty(&row_document(&columns, &row)).unwrap();
        let back = shell::parse_doc(&text).unwrap();
        assert_eq!(back, d, "{text}");
        assert_eq!(
            back.keys().collect::<Vec<_>>(),
            d.keys().collect::<Vec<_>>()
        );
        let nested = back.get_document("nested").unwrap();
        assert_eq!(nested.keys().collect::<Vec<_>>(), ["z", "a", "n"]);
        // Each value alone, from its edit text, through the typed edit.
        for (k, b) in &d {
            let v = to_value(b);
            let edited = typed_edit(&v, &Edit::Text(v.edit_text())).unwrap();
            let expected = if b == &Bson::Null {
                Bson::String(String::new())
            } else {
                b.clone()
            };
            assert_eq!(edited, expected, "field {k}: {}", v.edit_text());
        }
        // mongosh preview text also parses back.
        assert_eq!(
            shell::parse_doc(&shell_doc(&d)).unwrap(),
            d,
            "{}",
            shell_doc(&d)
        );
    }

    #[test]
    fn typed_edits() {
        let t = |orig: Value, s: &str| typed_edit(&orig, &Edit::Text(s.into()));
        let oid = Value::Json(serde_json::json!({"$oid": "64b7f0c2a1b2c3d4e5f60718"}));
        let date = Value::Json(serde_json::json!({"$date": {"$numberLong": "0"}}));
        let long = Value::Json(serde_json::json!({"$numberLong": "1"}));
        let dec = Value::Json(serde_json::json!({"$numberDecimal": "1"}));
        assert_eq!(
            t(Value::Text("a".into()), " 42 ").unwrap(),
            Bson::String(" 42 ".into())
        );
        assert_eq!(t(Value::Int(1), "42").unwrap(), Bson::Int32(42));
        assert_eq!(
            t(Value::Int(1), "3000000000").unwrap(),
            Bson::Int64(3_000_000_000)
        );
        assert!(t(Value::Int(1), "4.5").is_err());
        assert_eq!(t(Value::Float(1.0), "4").unwrap(), Bson::Double(4.0));
        assert_eq!(t(Value::Bool(true), "false").unwrap(), Bson::Boolean(false));
        assert!(t(Value::Bool(true), "nope").is_err());
        assert_eq!(t(long.clone(), "7").unwrap(), Bson::Int64(7));
        assert_eq!(t(long, "NumberLong(8)").unwrap(), Bson::Int64(8));
        assert_eq!(
            t(dec, "1.25").unwrap(),
            Bson::Decimal128("1.25".parse().unwrap())
        );
        let want = Bson::ObjectId(ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60719").unwrap());
        assert_eq!(t(oid.clone(), "64b7f0c2a1b2c3d4e5f60719").unwrap(), want);
        assert_eq!(
            t(oid.clone(), "ObjectId('64b7f0c2a1b2c3d4e5f60719')").unwrap(),
            want
        );
        assert!(t(oid, "xyz").unwrap_err().to_string().contains("24 hex"));
        let ms = |b: Bson| b.as_datetime().unwrap().timestamp_millis();
        assert_eq!(ms(t(date.clone(), "1970-01-01T00:00:01Z").unwrap()), 1000);
        assert_eq!(
            ms(t(date.clone(), "ISODate(\"1970-01-01T00:00:02Z\")").unwrap()),
            2000
        );
        assert_eq!(ms(t(date.clone(), "3000").unwrap()), 3000);
        assert!(t(date, "soon").is_err());
        assert_eq!(
            t(Value::Json(serde_json::json!({"a": 1})), "{b: 2}").unwrap(),
            Bson::Document(doc! {"b": 2})
        );
        assert_eq!(
            t(Value::Null, "{b: 2}").unwrap(),
            Bson::Document(doc! {"b": 2})
        );
        assert_eq!(t(Value::Missing, "12").unwrap(), Bson::Int32(12));
        assert_eq!(
            t(Value::Missing, "hello world").unwrap(),
            Bson::String("hello world".into())
        );
        assert_eq!(typed_edit(&Value::Int(1), &Edit::Null).unwrap(), Bson::Null);
    }

    #[test]
    fn shell_texts() {
        let d = doc! {"_id": ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap(), "a-b": "it's", "n": Bson::Int64(5), "f": 1.0, "at": DateTime::from_millis(0)};
        assert_eq!(
            shell_doc(&d),
            "{_id: ObjectId('64b7f0c2a1b2c3d4e5f60718'), 'a-b': 'it\\'s', n: NumberLong(5), f: 1.0, at: ISODate('1970-01-01T00:00:00.000Z')}"
        );
        assert_eq!(coll_text("users"), "db.users");
        assert_eq!(coll_text("system.profile"), "db.system.profile");
        assert_eq!(coll_text("my-coll"), "db.getCollection('my-coll')");
    }

    #[test]
    fn columns_and_types() {
        let docs = vec![
            doc! {"b": 1, "_id": 1},
            doc! {"_id": 2, "c": "x", "b": "y"},
            doc! {"b": 2},
        ];
        let cols = union_columns(&docs);
        assert_eq!(cols, ["_id", "b", "c"]);
        let rs = docs_result(&docs, cols);
        assert_eq!(rs.types, ["Int32", "Int32", "String"]);
        assert_eq!(
            rs.rows[2],
            vec![Value::Missing, Value::Int(2), Value::Missing]
        );
    }
}
