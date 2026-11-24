//! SQL text helpers: quoting, statement split, and safety checks.

use crate::model::Kind;

/// Quote an identifier: `"name"` (PostgreSQL, SQLite) or `` `name` `` (MySQL).
pub fn quote_ident(kind: Kind, name: &str) -> String {
    match kind {
        Kind::MySql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// `namespace.name`, both quoted.
pub fn qualified(kind: Kind, namespace: &str, name: &str) -> String {
    format!(
        "{}.{}",
        quote_ident(kind, namespace),
        quote_ident(kind, name)
    )
}

/// PostgreSQL string literal. With `standard_conforming_strings = off` it uses
/// the `E''` form and escapes backslashes.
pub fn pg_literal(s: &str, standard_strings: bool) -> String {
    if standard_strings {
        format!("'{}'", s.replace('\'', "''"))
    } else {
        format!("E'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
    }
}

/// PostgreSQL bytea literal.
pub fn pg_bytea_literal(b: &[u8], standard_strings: bool) -> String {
    format!(
        "{}::bytea",
        pg_literal(&format!("\\x{}", crate::value::hex(b)), standard_strings)
    )
}

/// MySQL string literal. `backslash_escapes` is false when `sql_mode`
/// contains NO_BACKSLASH_ESCAPES.
pub fn mysql_literal(s: &str, backslash_escapes: bool) -> String {
    if !backslash_escapes {
        return format!("'{}'", s.replace('\'', "''"));
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x1a' => out.push_str("\\Z"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// SQLite string literal.
pub fn sqlite_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `X'..'` blob literal (MySQL, SQLite).
pub fn x_literal(b: &[u8]) -> String {
    format!("X'{}'", crate::value::hex(b))
}

/// Split SQL text into statements at `;`. Quotes ('', "", ``), comments
/// (`--`, `#` for MySQL is not handled, `/* */`) and PostgreSQL dollar quotes
/// are respected. Empty statements are dropped.
pub fn split_statements(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if i + 1 < b.len() && b[i + 1] == q {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    if b[i] == b'\\' && q == b'\'' {
                        i += 1; // MySQL-style escape; harmless for others in practice
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'$' => {
                // $tag$ ... $tag$
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j < b.len()
                    && b[j] == b'$'
                    && !b[i + 1..j].first().is_some_and(u8::is_ascii_digit)
                {
                    let tag = &text[i..=j];
                    match text[j + 1..].find(tag) {
                        Some(end) => i = j + 1 + end + tag.len(),
                        None => i = b.len(),
                    }
                } else {
                    i += 1;
                }
            }
            b';' => {
                let s = text[start..i].trim();
                if !s.is_empty() {
                    out.push(s);
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    let s = text[start.min(text.len())..].trim();
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// Upper-case words of a statement outside quotes and comments.
fn words(stmt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = stmt.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                flush(&mut cur, &mut out);
            }
            '-' if chars.peek() == Some(&'-') => {
                flush(&mut cur, &mut out);
                for c2 in chars.by_ref() {
                    if c2 == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                flush(&mut cur, &mut out);
                chars.next();
                let mut prev = ' ';
                for c2 in chars.by_ref() {
                    if prev == '*' && c2 == '/' {
                        break;
                    }
                    prev = c2;
                }
            }
            c if c.is_alphanumeric() || c == '_' => cur.push(c.to_ascii_uppercase()),
            _ => flush(&mut cur, &mut out),
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn flush(cur: &mut String, out: &mut Vec<String>) {
    if !cur.is_empty() {
        out.push(std::mem::take(cur));
    }
}

const DOC_WRITE_METHODS: [&str; 14] = [
    ".insertOne(",
    ".insertMany(",
    ".updateOne(",
    ".updateMany(",
    ".replaceOne(",
    ".deleteOne(",
    ".deleteMany(",
    ".drop(",
    ".dropDatabase(",
    ".createIndex(",
    ".dropIndex(",
    ".renameCollection(",
    ".runCommand(",
    ".remove(",
];

/// A reason to ask before running, or `None`. SQL: DROP, TRUNCATE, ALTER … DROP,
/// DELETE/UPDATE without WHERE. Documents: drop, dropDatabase, deleteMany({}),
/// updateMany({}, …).
pub fn risky(text: &str, document: bool) -> Option<String> {
    if document {
        let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        for (pat, why) in [
            (".dropDatabase(", "drops a database"),
            (".drop(", "drops a collection"),
            (".deleteMany({})", "deletes every document"),
            (".deleteMany()", "deletes every document"),
            (".updateMany({},", "updates every document"),
            (".remove({})", "deletes every document"),
        ] {
            if t.contains(pat) {
                return Some(format!("This command {why}."));
            }
        }
        return None;
    }
    for stmt in split_statements(text) {
        let w = words(stmt);
        let first = w.first().map(String::as_str).unwrap_or("");
        let has = |k: &str| w.iter().any(|x| x == k);
        let reason = match first {
            "DROP" => Some("DROP"),
            "TRUNCATE" => Some("TRUNCATE"),
            "ALTER" if has("DROP") => Some("ALTER … DROP"),
            "DELETE" if !has("WHERE") => Some("DELETE without WHERE"),
            "UPDATE" if !has("WHERE") => Some("UPDATE without WHERE"),
            _ => None,
        };
        if let Some(r) = reason {
            let snippet: String = stmt.chars().take(80).collect();
            return Some(format!("{r}: {snippet}"));
        }
    }
    None
}

/// True when the text can change data or schema (used to ask on Prod).
pub fn is_write(text: &str, document: bool) -> bool {
    if document {
        let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        return DOC_WRITE_METHODS.iter().any(|m| t.contains(m));
    }
    const READ: [&str; 14] = [
        "SELECT", "WITH", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "VALUES", "TABLE", "USE", "SET",
        "BEGIN", "START", "COMMIT", "ROLLBACK",
    ];
    split_statements(text).into_iter().any(|stmt| {
        let w = words(stmt);
        let first = w.first().map(String::as_str).unwrap_or("");
        if first == "WITH" {
            return w
                .iter()
                .any(|x| matches!(x.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE"));
        }
        !READ.contains(&first)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_ident(Kind::Postgres, r#"a"b"#), r#""a""b""#);
        assert_eq!(quote_ident(Kind::MySql, "a`b"), "`a``b`");
        assert_eq!(pg_literal("O'Brien \\", true), r"'O''Brien \'");
        assert_eq!(pg_literal("O'Brien \\", false), r"E'O''Brien \\'");
        assert_eq!(mysql_literal("a'b\\c\n", true), r"'a\'b\\c\n'");
        assert_eq!(mysql_literal("a'b\\c", false), r"'a''b\c'");
        assert_eq!(pg_bytea_literal(&[1, 255], true), r"'\x01ff'::bytea");
    }

    #[test]
    fn split() {
        let s = "select ';' ; select $$a;b$$; -- x;y\nselect 1 /* ; */";
        assert_eq!(
            split_statements(s),
            vec!["select ';'", "select $$a;b$$", "-- x;y\nselect 1 /* ; */"]
        );
        assert_eq!(split_statements(" ; ;"), Vec::<&str>::new());
    }

    #[test]
    fn safety() {
        assert!(risky("delete from t", false).is_some());
        assert!(risky("delete from t where id = 1", false).is_none());
        assert!(risky("update t set a = 'where'", false).is_some());
        assert!(risky("select 1; drop table x", false).is_some());
        assert!(risky("db.users.deleteMany({ })", true).is_some());
        assert!(risky("db.users.deleteMany({a: 1})", true).is_none());
        assert!(!is_write("select 1; show tables", false));
        assert!(is_write(
            "with x as (delete from t returning *) select * from x",
            false
        ));
        assert!(is_write("insert into t values (1)", false));
        assert!(is_write("db.t.insertOne({a: 1})", true));
        assert!(!is_write("db.t.find({})", true));
    }
}
