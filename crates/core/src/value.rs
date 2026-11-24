//! Cell values and their text forms.
//!
//! SQL drivers give `Null` or `Text` for most cells (the text the server sends),
//! `Bytes` for binary data, and sometimes `Int`/`Float`/`Bool`.
//! The MongoDB driver gives `Null`, `Missing`, `Bool`, `Int` (Int32), `Float`
//! (Double), `Text` (String), and `Json` with canonical Extended JSON for every
//! other BSON type (ObjectId, Date, Int64, Decimal128, documents, arrays, ...).

use base64::Engine;
use serde_json::{Map, Number, Value as J};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// SQL NULL or BSON null.
    Null,
    /// The field is not in this document (documents only).
    Missing,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// Canonical Extended JSON (documents only).
    Json(J),
}

/// Max characters of a grid cell.
const CELL_MAX: usize = 300;

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn is_missing(&self) -> bool {
        matches!(self, Value::Missing)
    }

    /// True for numbers (right-aligned in the grid).
    pub fn is_number(&self) -> bool {
        match self {
            Value::Int(_) | Value::Float(_) => true,
            Value::Json(J::Object(m)) if m.len() == 1 => {
                let k = m.keys().next().map(String::as_str).unwrap_or("");
                matches!(
                    k,
                    "$numberLong" | "$numberInt" | "$numberDouble" | "$numberDecimal"
                )
            }
            _ => false,
        }
    }

    /// One line for a grid cell: no line breaks, cut at 300 characters.
    pub fn display(&self) -> String {
        let s = match self {
            Value::Null => return "NULL".into(),
            Value::Missing => return String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => fmt_float(*f),
            Value::Text(s) => s.clone(),
            Value::Bytes(b) => hex_preview(b),
            Value::Json(j) => ejson_inline(j),
        };
        one_line(&s, CELL_MAX)
    }

    /// Full text for the value panel. JSON is pretty-printed.
    pub fn display_full(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Missing => String::new(),
            Value::Bytes(b) => format!("0x{}", hex(b)),
            Value::Json(j) => match scalar_text(j) {
                Some(_) => ejson_inline(j),
                None => serde_json::to_string_pretty(&ejson_relax(j)).unwrap_or_default(),
            },
            Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Text(_) => self.edit_text(),
        }
    }

    /// The text an editor starts with. `Null` and `Missing` give "".
    /// For a Mongo ObjectId this is the bare hex, for a Date the ISO text, etc.
    pub fn edit_text(&self) -> String {
        match self {
            Value::Null | Value::Missing => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => fmt_float(*f),
            Value::Text(s) => s.clone(),
            Value::Bytes(b) => format!("0x{}", hex(b)),
            Value::Json(j) => scalar_text(j).unwrap_or_else(|| {
                serde_json::to_string_pretty(&ejson_relax(j)).unwrap_or_default()
            }),
        }
    }

    /// Relaxed-style JSON for export and for document editing. `Json` values
    /// keep the type wrappers that plain JSON cannot hold ($oid, $numberLong,
    /// $numberDecimal, $binary, ...), so the text parses back to the same BSON.
    pub fn to_json(&self) -> J {
        match self {
            Value::Null | Value::Missing => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Int(i) => J::from(*i),
            // NaN and ±Infinity have no plain JSON form: keep them as EJSON doubles.
            Value::Float(f) => Number::from_f64(*f).map(J::Number).unwrap_or_else(|| {
                let s = if f.is_nan() {
                    "NaN"
                } else if *f > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                };
                serde_json::json!({ "$numberDouble": s })
            }),
            Value::Text(s) => J::String(s.clone()),
            Value::Bytes(b) => J::String(format!("0x{}", hex(b))),
            Value::Json(j) => ejson_relax(j),
        }
    }
}

/// Build a document from one grid row (fields that are `Missing` are left out).
pub fn row_document(columns: &[String], row: &[Value]) -> J {
    let mut m = Map::new();
    for (c, v) in columns.iter().zip(row) {
        if !v.is_missing() {
            m.insert(c.clone(), v.to_json());
        }
    }
    J::Object(m)
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn hex_preview(b: &[u8]) -> String {
    if b.len() <= 32 {
        format!("0x{}", hex(b))
    } else {
        format!("0x{}… ({} bytes)", hex(&b[..32]), b.len())
    }
}

fn fmt_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

fn one_line(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i == max {
            out.push('…');
            break;
        }
        out.push(if matches!(ch, '\n' | '\r' | '\t') {
            ' '
        } else {
            ch
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Extended JSON helpers
// ---------------------------------------------------------------------------

fn single(j: &J) -> Option<(&str, &J)> {
    match j {
        J::Object(m) if m.len() == 1 => m.iter().next().map(|(k, v)| (k.as_str(), v)),
        _ => None,
    }
}

/// Bare text of a scalar EJSON wrapper ($oid, $date, $numberLong, ...).
fn scalar_text(j: &J) -> Option<String> {
    let (k, v) = single(j)?;
    match (k, v) {
        ("$oid", J::String(s)) => Some(s.clone()),
        ("$numberLong" | "$numberInt" | "$numberDouble" | "$numberDecimal", J::String(s)) => {
            Some(s.clone())
        }
        ("$date", J::String(s)) => Some(s.clone()),
        ("$date", J::Object(_)) => {
            let ms = single(v).and_then(|(k2, v2)| (k2 == "$numberLong").then_some(v2))?;
            ms.as_str()?.parse::<i64>().ok().map(iso_from_millis)
        }
        ("$date", J::Number(n)) => n.as_i64().map(iso_from_millis),
        _ => None,
    }
}

/// One-line text for an EJSON value: `ObjectId('…')`, ISO dates, plain numbers,
/// compact JSON for documents and arrays.
pub fn ejson_inline(j: &J) -> String {
    if let Some((k, v)) = single(j) {
        match k {
            "$oid" => return format!("ObjectId('{}')", v.as_str().unwrap_or_default()),
            "$date" | "$numberLong" | "$numberInt" | "$numberDouble" | "$numberDecimal" => {
                if let Some(s) = scalar_text(j) {
                    return s;
                }
            }
            "$binary" => {
                let b64 = v.get("base64").and_then(J::as_str).unwrap_or_default();
                let st = v.get("subType").and_then(J::as_str).unwrap_or("00");
                return format!("Binary('{b64}', {st})");
            }
            "$timestamp" => {
                let t = v.get("t").and_then(J::as_u64).unwrap_or(0);
                let i = v.get("i").and_then(J::as_u64).unwrap_or(0);
                return format!("Timestamp({t}, {i})");
            }
            "$regularExpression" => {
                let p = v.get("pattern").and_then(J::as_str).unwrap_or_default();
                let o = v.get("options").and_then(J::as_str).unwrap_or_default();
                return format!("/{p}/{o}");
            }
            "$minKey" => return "MinKey".into(),
            "$maxKey" => return "MaxKey".into(),
            "$symbol" | "$code" => return v.as_str().unwrap_or_default().to_string(),
            _ => {}
        }
    }
    serde_json::to_string(&ejson_relax(j)).unwrap_or_default()
}

/// Canonical EJSON to a readable form that still parses back to the same BSON:
/// `$numberInt` and finite `$numberDouble` become plain numbers, and
/// `$date` with `$numberLong` (years 1970–9999) becomes `{"$date": "<ISO>"}`.
/// All other wrappers stay.
pub fn ejson_relax(j: &J) -> J {
    if let Some((k, v)) = single(j) {
        match (k, v) {
            ("$numberInt", J::String(s)) => {
                if let Ok(i) = s.parse::<i64>() {
                    return J::from(i);
                }
            }
            ("$numberDouble", J::String(s)) => {
                if let Some(n) = s
                    .parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite())
                    .and_then(Number::from_f64)
                {
                    return J::Number(n);
                }
            }
            ("$date", J::Object(_)) => {
                if let Some(ms) = single(v)
                    .filter(|(k2, _)| *k2 == "$numberLong")
                    .and_then(|(_, v2)| v2.as_str()?.parse::<i64>().ok())
                    .filter(|ms| (0..=253_402_300_799_999).contains(ms))
                {
                    let mut m = Map::new();
                    m.insert("$date".into(), J::String(iso_from_millis(ms)));
                    return J::Object(m);
                }
                return j.clone();
            }
            _ if k.starts_with('$') => return j.clone(),
            _ => {}
        }
    }
    match j {
        J::Object(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), ejson_relax(v))).collect()),
        J::Array(a) => J::Array(a.iter().map(ejson_relax).collect()),
        _ => j.clone(),
    }
}

/// Milliseconds since the Unix epoch to "YYYY-MM-DDTHH:MM:SS.mmmZ".
pub fn iso_from_millis(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // Civil-from-days (H. Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Base64 (standard alphabet) helper for drivers.
pub fn base64_encode(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iso_dates() {
        assert_eq!(iso_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso_from_millis(1_700_000_000_123),
            "2023-11-14T22:13:20.123Z"
        );
        assert_eq!(iso_from_millis(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(iso_from_millis(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn ejson_forms() {
        let oid = Value::Json(json!({"$oid": "64b7f0c2a1b2c3d4e5f60718"}));
        assert_eq!(oid.display(), "ObjectId('64b7f0c2a1b2c3d4e5f60718')");
        assert_eq!(oid.edit_text(), "64b7f0c2a1b2c3d4e5f60718");
        let date = Value::Json(json!({"$date": {"$numberLong": "0"}}));
        assert_eq!(date.display(), "1970-01-01T00:00:00.000Z");
        assert_eq!(date.to_json(), json!({"$date": "1970-01-01T00:00:00.000Z"}));
        let long = Value::Json(json!({"$numberLong": "42"}));
        assert!(long.is_number());
        assert_eq!(long.display(), "42");
        assert_eq!(long.to_json(), json!({"$numberLong": "42"}));
        let doc = Value::Json(json!({"a": {"$numberInt": "1"}, "b": [{"$numberDouble": "2.5"}]}));
        assert_eq!(doc.display(), r#"{"a":1,"b":[2.5]}"#);
        // Field order stays as stored (serde_json preserve_order).
        let ordered = Value::Json(json!({"z": 1, "a": 2}));
        assert_eq!(ordered.display(), r#"{"z":1,"a":2}"#);
        assert_eq!(
            Value::Float(f64::NAN).to_json(),
            json!({"$numberDouble": "NaN"})
        );
        assert_eq!(
            Value::Float(f64::NEG_INFINITY).to_json(),
            json!({"$numberDouble": "-Infinity"})
        );
    }

    #[test]
    fn cells() {
        assert_eq!(Value::Text("a\nb".into()).display(), "a b");
        assert_eq!(Value::Float(3.0).display(), "3.0");
        assert_eq!(Value::Bytes(vec![0xde, 0xad]).display(), "0xdead");
        assert_eq!(Value::Null.edit_text(), "");
        let long = "x".repeat(400);
        assert_eq!(Value::Text(long).display().chars().count(), 301);
    }
}
