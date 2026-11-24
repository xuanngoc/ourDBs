//! Turn MySQL text-protocol cells into OurDB values, and name column types.

use mysql_async::{
    Column, Value as MyValue,
    consts::{ColumnFlags, ColumnType},
};
use ourdb_core::Value;

/// Character set number of binary strings and of non-string types.
const BINARY_CHARSET: u16 = 63;

/// Type names that `type_name` gives to binary columns (see `is_binary`).
const BINARY_TYPE_NAMES: [&str; 5] = ["binary", "varbinary", "blob", "bit", "geometry"];

/// True when the column holds raw bytes: a BLOB, BINARY, VARBINARY, BIT or
/// GEOMETRY type with the binary character set. Numbers and dates also use
/// charset 63, so the type decides too.
pub fn is_binary(col: &Column) -> bool {
    use ColumnType::*;
    let binary_type = matches!(
        col.column_type(),
        MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_BLOB
            | MYSQL_TYPE_STRING
            | MYSQL_TYPE_VAR_STRING
            | MYSQL_TYPE_VARCHAR
            | MYSQL_TYPE_BIT
            | MYSQL_TYPE_GEOMETRY
    );
    let flags = col.flags();
    binary_type
        && col.character_set() == BINARY_CHARSET
        && !flags.intersects(ColumnFlags::ENUM_FLAG | ColumnFlags::SET_FLAG)
}

/// True for a name that `type_name` gives only to binary columns.
pub fn is_binary_type_name(name: &str) -> bool {
    BINARY_TYPE_NAMES.contains(&name)
}

/// A short type name from the column metadata, for example "int unsigned",
/// "varchar" or "blob". Empty when the type is unknown.
pub fn type_name(col: &Column) -> String {
    use ColumnType::*;
    let flags = col.flags();
    let binary = is_binary(col);
    let name = match col.column_type() {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_NULL => "null",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_VECTOR => "vector",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_BIT if binary => "bit",
        MYSQL_TYPE_GEOMETRY if binary => "geometry",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "blob"
            } else {
                "text"
            }
        }
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => {
            if binary {
                "varbinary"
            } else {
                "varchar"
            }
        }
        MYSQL_TYPE_STRING if flags.contains(ColumnFlags::ENUM_FLAG) => "enum",
        MYSQL_TYPE_STRING if flags.contains(ColumnFlags::SET_FLAG) => "set",
        MYSQL_TYPE_STRING => {
            if binary {
                "binary"
            } else {
                "char"
            }
        }
        _ => "",
    };
    // YEAR also has the UNSIGNED flag, so list the number types here.
    let numeric = matches!(
        col.column_type(),
        MYSQL_TYPE_TINY
            | MYSQL_TYPE_SHORT
            | MYSQL_TYPE_INT24
            | MYSQL_TYPE_LONG
            | MYSQL_TYPE_LONGLONG
            | MYSQL_TYPE_DECIMAL
            | MYSQL_TYPE_NEWDECIMAL
            | MYSQL_TYPE_FLOAT
            | MYSQL_TYPE_DOUBLE
    );
    if numeric && flags.contains(ColumnFlags::UNSIGNED_FLAG) {
        format!("{name} unsigned")
    } else {
        name.to_string()
    }
}

/// Convert one row. `binary[i]` tells if column `i` holds raw bytes.
pub fn row_values(row: mysql_async::Row, binary: &[bool]) -> Vec<Value> {
    row.unwrap()
        .into_iter()
        .enumerate()
        .map(|(i, v)| to_value(v, binary.get(i).copied().unwrap_or(false)))
        .collect()
}

/// Convert one cell. The text protocol sends `NULL` or `Bytes`. The other
/// variants come only from the binary protocol; they are handled just in case.
pub fn to_value(v: MyValue, binary: bool) -> Value {
    match v {
        MyValue::NULL => Value::Null,
        MyValue::Bytes(b) if binary => Value::Bytes(b),
        MyValue::Bytes(b) => Value::Text(utf8_lossy(b)),
        MyValue::Int(i) => Value::Text(i.to_string()),
        MyValue::UInt(u) => Value::Text(u.to_string()),
        MyValue::Float(f) => Value::Text(f.to_string()),
        MyValue::Double(d) => Value::Text(d.to_string()),
        MyValue::Date(y, mo, d, h, mi, s, us) => {
            let mut t = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}");
            if us > 0 {
                t.push_str(&format!(".{us:06}"));
            }
            Value::Text(t)
        }
        MyValue::Time(neg, days, h, mi, s, us) => {
            let sign = if neg { "-" } else { "" };
            let hours = u64::from(days) * 24 + u64::from(h);
            let mut t = format!("{sign}{hours:02}:{mi:02}:{s:02}");
            if us > 0 {
                t.push_str(&format!(".{us:06}"));
            }
            Value::Text(t)
        }
    }
}

/// UTF-8 text from bytes. Invalid sequences become U+FFFD.
fn utf8_lossy(b: Vec<u8>) -> String {
    String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(t: ColumnType, charset: u16, flags: ColumnFlags) -> Column {
        Column::new(t).with_character_set(charset).with_flags(flags)
    }

    #[test]
    fn binary_detection() {
        let none = ColumnFlags::empty();
        assert!(is_binary(&col(
            ColumnType::MYSQL_TYPE_BLOB,
            63,
            ColumnFlags::BLOB_FLAG
        )));
        assert!(is_binary(&col(ColumnType::MYSQL_TYPE_VAR_STRING, 63, none)));
        assert!(is_binary(&col(ColumnType::MYSQL_TYPE_BIT, 63, none)));
        // TEXT has a real charset; numbers and JSON use 63 but are not binary.
        assert!(!is_binary(&col(
            ColumnType::MYSQL_TYPE_BLOB,
            255,
            ColumnFlags::BLOB_FLAG
        )));
        assert!(!is_binary(&col(ColumnType::MYSQL_TYPE_LONGLONG, 63, none)));
        assert!(!is_binary(&col(ColumnType::MYSQL_TYPE_JSON, 63, none)));
        assert!(!is_binary(&col(ColumnType::MYSQL_TYPE_DATETIME, 63, none)));
    }

    #[test]
    fn names_agree_with_binary_flag() {
        use ColumnType::*;
        let types = [
            MYSQL_TYPE_TINY,
            MYSQL_TYPE_LONGLONG,
            MYSQL_TYPE_NEWDECIMAL,
            MYSQL_TYPE_BLOB,
            MYSQL_TYPE_STRING,
            MYSQL_TYPE_VAR_STRING,
            MYSQL_TYPE_BIT,
            MYSQL_TYPE_GEOMETRY,
            MYSQL_TYPE_JSON,
            MYSQL_TYPE_DATE,
        ];
        let flag_sets = [
            ColumnFlags::empty(),
            ColumnFlags::ENUM_FLAG,
            ColumnFlags::UNSIGNED_FLAG,
        ];
        for t in types {
            for charset in [63, 255] {
                for flags in flag_sets {
                    let c = col(t, charset, flags);
                    assert_eq!(
                        is_binary(&c),
                        is_binary_type_name(&type_name(&c)),
                        "{t:?} {charset} {flags:?}"
                    );
                }
            }
        }
        let c = col(MYSQL_TYPE_LONGLONG, 63, ColumnFlags::UNSIGNED_FLAG);
        assert_eq!(type_name(&c), "bigint unsigned");
        assert_eq!(
            type_name(&col(MYSQL_TYPE_BLOB, 255, ColumnFlags::empty())),
            "text"
        );
        assert_eq!(
            type_name(&col(MYSQL_TYPE_STRING, 255, ColumnFlags::ENUM_FLAG)),
            "enum"
        );
    }

    #[test]
    fn values() {
        assert_eq!(to_value(MyValue::NULL, false), Value::Null);
        assert_eq!(
            to_value(MyValue::Bytes(b"".to_vec()), false),
            Value::Text(String::new())
        );
        assert_eq!(
            to_value(MyValue::Bytes(vec![0xff]), true),
            Value::Bytes(vec![0xff])
        );
        assert_eq!(
            to_value(MyValue::Bytes(vec![b'a', 0xff]), false),
            Value::Text("a\u{fffd}".into())
        );
        assert_eq!(
            to_value(MyValue::Date(2024, 2, 29, 1, 2, 3, 0), false),
            Value::Text("2024-02-29 01:02:03".into())
        );
        assert_eq!(
            to_value(MyValue::Time(true, 1, 2, 3, 4, 5), false),
            Value::Text("-26:03:04.000005".into())
        );
    }
}
