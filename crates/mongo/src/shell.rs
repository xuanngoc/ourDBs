//! Shell syntax: mongosh-style JSON and the query editor subset (SPEC section 6).
//!
//! Shell JSON is JSON5 plus the helpers `ObjectId("…")`, `ISODate("…")`,
//! `new Date(…)`, `NumberLong(…)`, `NumberInt(…)` and `NumberDecimal("…")`.
//! We rewrite the helpers to Extended JSON, parse the text with `json5`, and
//! convert the result with `Bson::try_from`. Key order is kept, because the
//! crate turns on the `preserve_order` feature of `serde_json`.

use anyhow::{Result, anyhow, bail};
use bson::{Bson, DateTime, Decimal128, Document, oid::ObjectId};

// ---------------------------------------------------------------------------
// Shell JSON
// ---------------------------------------------------------------------------

/// Parse one shell JSON value.
pub fn parse_value(text: &str) -> Result<Bson> {
    parse_shown(text, text)
}

/// Parse `text`. Error messages show `shown`, the text the user typed.
fn parse_shown(text: &str, shown: &str) -> Result<Bson> {
    let json_text = rewrite_helpers(text)?;
    let json: serde_json::Value =
        json5::from_str(&json_text).map_err(|e| syntax_error(shown, &json_text, &e))?;
    Bson::try_from(json)
        .map_err(|e| anyhow!("invalid value in `{}`: {}", snippet(shown), bson_error(&e)))
}

/// Parse a shell JSON document. Empty text gives an empty document.
pub fn parse_doc(text: &str) -> Result<Document> {
    if text.trim().is_empty() {
        return Ok(Document::new());
    }
    match parse_value(text)? {
        Bson::Document(d) => Ok(d),
        other => bail!(
            "expected a document like {{name: 'x'}}, got {} in `{}`",
            type_name(&other),
            snippet(text)
        ),
    }
}

/// Parse a comma-separated argument list, for example `{a: 1}, {b: 1}`.
pub fn parse_args(text: &str) -> Result<Vec<Bson>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    match parse_shown(&format!("[{text}\n]"), text)? {
        Bson::Array(a) => Ok(a),
        _ => unreachable!("a bracketed list always parses to an array"),
    }
}

/// Rewrite the shell helpers to Extended JSON. Strings and comments are copied
/// as they are.
fn rewrite_helpers(text: &str) -> Result<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let end = skip_string(&chars, i);
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        if let Some(end) = skip_comment(&chars, i) {
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        if is_ident_start(c) && (i == 0 || !is_ident_char(chars[i - 1])) {
            let word_end = ident_end(&chars, i);
            let word: String = chars[i..word_end].iter().collect();
            let is_key = chars.get(skip_space(&chars, word_end)) == Some(&':');
            if (word == "NaN" || word == "Infinity") && !is_key {
                // serde_json cannot hold NaN or Infinity (it makes them null),
                // so write them as Extended JSON doubles.
                let sign = if out.trim_end().ends_with('-') {
                    out.truncate(out.trim_end().len() - 1);
                    "-"
                } else {
                    ""
                };
                out.push_str(&format!(r#"{{"$numberDouble":"{sign}{word}"}}"#));
                i = word_end;
            } else if let Some((json, end)) = helper_at(&chars, i, word_end)? {
                out.push_str(&json);
                i = end;
            } else {
                out.extend(&chars[i..word_end]);
                i = word_end;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}

/// If a helper call starts at `start`, return its Extended JSON text and the
/// index after the closing parenthesis.
fn helper_at(chars: &[char], start: usize, word_end: usize) -> Result<Option<(String, usize)>> {
    let word: String = chars[start..word_end].iter().collect();
    let (name, name_end) = if word == "new" {
        let s = skip_space(chars, word_end);
        if s >= chars.len() || !is_ident_start(chars[s]) {
            return Ok(None);
        }
        let e = ident_end(chars, s);
        (chars[s..e].iter().collect::<String>(), e)
    } else {
        (word.clone(), word_end)
    };
    let known = matches!(
        name.as_str(),
        "ObjectId" | "ISODate" | "NumberLong" | "NumberInt" | "NumberDecimal"
    ) || (name == "Date" && word == "new");
    if !known {
        return Ok(None);
    }
    let open = skip_space(chars, name_end);
    if chars.get(open) != Some(&'(') {
        return Ok(None);
    }
    let close = matching_close(chars, open).ok_or_else(|| {
        anyhow!(
            "missing `)` after {name}( in `{}`",
            snippet(&chars[start..].iter().collect::<String>())
        )
    })?;
    let call: String = chars[start..=close].iter().collect();
    let arg = HelperArg::parse(&chars[open + 1..close].iter().collect::<String>(), &call)?;
    let json = helper_json(&name, arg, &call)?;
    Ok(Some((json, close + 1)))
}

/// The single argument of a helper call.
enum HelperArg {
    None,
    Str(String),
    Raw(String),
}

impl HelperArg {
    fn parse(inner: &str, call: &str) -> Result<HelperArg> {
        let t = inner.trim();
        if t.is_empty() {
            return Ok(HelperArg::None);
        }
        let chars: Vec<char> = t.chars().collect();
        if chars[0] == '"' || chars[0] == '\'' {
            let end = skip_string(&chars, 0);
            if end != chars.len() {
                bail!("{call}: expected one argument");
            }
            return Ok(HelperArg::Str(unquote(&chars)));
        }
        if t.contains(',') {
            bail!("{call}: expected one argument");
        }
        Ok(HelperArg::Raw(t.to_string()))
    }

    fn text(&self) -> Option<&str> {
        match self {
            HelperArg::None => None,
            HelperArg::Str(s) | HelperArg::Raw(s) => Some(s.trim()),
        }
    }
}

fn helper_json(name: &str, arg: HelperArg, call: &str) -> Result<String> {
    Ok(match name {
        "ObjectId" => {
            let oid = match &arg {
                HelperArg::None => ObjectId::new(),
                HelperArg::Str(s) => parse_object_id(s).map_err(|e| anyhow!("{call}: {e}"))?,
                HelperArg::Raw(_) => bail!("{call}: the hex text must be in quotes"),
            };
            format!(r#"{{"$oid":"{}"}}"#, oid.to_hex())
        }
        "ISODate" | "Date" => {
            let date = match &arg {
                HelperArg::None => DateTime::now(),
                HelperArg::Str(s) => parse_date(s).map_err(|e| anyhow!("{call}: {e}"))?,
                HelperArg::Raw(s) => DateTime::from_millis(parse_millis(s).ok_or_else(|| {
                    anyhow!("{call}: expected a date text or epoch milliseconds")
                })?),
            };
            format!(
                r#"{{"$date":{{"$numberLong":"{}"}}}}"#,
                date.timestamp_millis()
            )
        }
        "NumberLong" => {
            let n = arg.text().unwrap_or("0");
            let n: i64 = n
                .parse()
                .map_err(|_| anyhow!("{call}: expected a 64-bit integer"))?;
            format!(r#"{{"$numberLong":"{n}"}}"#)
        }
        "NumberInt" => {
            let n = arg.text().unwrap_or("0");
            let n: i32 = n
                .parse()
                .map_err(|_| anyhow!("{call}: expected a 32-bit integer"))?;
            format!(r#"{{"$numberInt":"{n}"}}"#)
        }
        "NumberDecimal" => {
            let n = arg.text().unwrap_or("0");
            n.parse::<Decimal128>()
                .map_err(|_| anyhow!("{call}: expected a decimal number"))?;
            format!(r#"{{"$numberDecimal":"{n}"}}"#)
        }
        _ => unreachable!("helper names are checked by the caller"),
    })
}

/// 24 hex characters to an ObjectId.
pub fn parse_object_id(text: &str) -> Result<ObjectId> {
    let t = text.trim();
    ObjectId::parse_str(t)
        .map_err(|_| anyhow!("invalid ObjectId `{t}`: expected 24 hex characters"))
}

/// ISO 8601 / RFC 3339 text to a date. A missing time means midnight, and a
/// missing time zone means UTC (like mongosh).
pub fn parse_date(text: &str) -> Result<DateTime> {
    let t = text.trim();
    let mut s = t.to_string();
    if s.len() == 10 {
        s.push_str("T00:00:00Z");
    }
    if s.len() > 10 && s.as_bytes()[10] == b' ' {
        s.replace_range(10..11, "T");
    }
    if let Some(tpos) = s.find(['T', 't']) {
        let (date, time) = s.split_at(tpos + 1);
        let (clock, zone) = match time.find(['Z', 'z', '+', '-']) {
            Some(p) => time.split_at(p),
            None => (time, ""),
        };
        let mut clock = clock.to_string();
        if clock.matches(':').count() == 1 {
            clock.push_str(":00");
        }
        let mut zone = zone.to_string();
        if zone.is_empty() {
            zone.push('Z');
        } else if zone.len() == 5 && !zone.contains(':') {
            zone.insert(3, ':'); // +0900 -> +09:00
        } else if zone.len() == 3 {
            zone.push_str(":00"); // +09 -> +09:00
        }
        s = format!("{date}{clock}{zone}");
    }
    DateTime::parse_rfc3339_str(&s)
        .map_err(|_| anyhow!("invalid date `{t}`: use ISO 8601, for example 2024-05-01T10:00:00Z"))
}

/// Epoch milliseconds (an integer, or a float without a fraction).
pub fn parse_millis(text: &str) -> Option<i64> {
    let t = text.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(n);
    }
    let f: f64 = t.parse().ok()?;
    (f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18).then_some(f as i64)
}

fn syntax_error(original: &str, json_text: &str, e: &json5::Error) -> anyhow::Error {
    let what = e.to_string();
    let what = match what.find(" at line ") {
        Some(p) => &what[..p],
        None => what.as_str(),
    };
    let near = e.position().and_then(|p| {
        let line = json_text.lines().nth(p.line)?;
        let rest: String = line.chars().skip(p.column).take(30).collect();
        (!rest.trim().is_empty()).then_some(rest)
    });
    match near {
        Some(near) => anyhow!(
            "syntax error: {what} near `{}` in `{}`",
            near.trim_end(),
            snippet(original)
        ),
        None => anyhow!("syntax error: {what} in `{}`", snippet(original)),
    }
}

/// The bson error text without the "BSON error. Kind: " noise.
fn bson_error(e: &bson::error::Error) -> String {
    let s = e.to_string();
    match s.find("Message: ") {
        Some(p) => s[p + 9..].to_string(),
        None => s.trim_start_matches("BSON error. Kind: ").to_string(),
    }
}

/// The first 80 characters of a text, on one line.
pub fn snippet(text: &str) -> String {
    cut(text, 80)
}

/// The first `max` characters of a text, on one line.
pub fn cut(text: &str, max: usize) -> String {
    let t = text.trim();
    let mut s: String = t
        .chars()
        .take(max)
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    if t.chars().count() > max {
        s.push('…');
    }
    s
}

/// mongosh-style type name of a BSON value.
pub fn type_name(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) => "Double",
        Bson::String(_) => "String",
        Bson::Array(_) => "Array",
        Bson::Document(_) => "Document",
        Bson::Boolean(_) => "Boolean",
        Bson::Null => "Null",
        Bson::RegularExpression(_) => "RegExp",
        Bson::JavaScriptCode(_) => "Code",
        Bson::JavaScriptCodeWithScope(_) => "CodeWithScope",
        Bson::Int32(_) => "Int32",
        Bson::Int64(_) => "Int64",
        Bson::Timestamp(_) => "Timestamp",
        Bson::Binary(_) => "Binary",
        Bson::ObjectId(_) => "ObjectId",
        Bson::DateTime(_) => "Date",
        Bson::Symbol(_) => "Symbol",
        Bson::Decimal128(_) => "Decimal128",
        Bson::Undefined => "Undefined",
        Bson::MaxKey => "MaxKey",
        Bson::MinKey => "MinKey",
        Bson::DbPointer(_) => "DBPointer",
    }
}

// ---------------------------------------------------------------------------
// Character scanning
// ---------------------------------------------------------------------------

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn ident_end(chars: &[char], start: usize) -> usize {
    let mut i = start;
    while i < chars.len() && is_ident_char(chars[i]) {
        i += 1;
    }
    i
}

fn skip_space(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Index after the string literal that starts at `start` (a quote).
/// An unclosed string runs to the end of the text.
fn skip_string(chars: &[char], start: usize) -> usize {
    let q = chars[start];
    let mut i = start + 1;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == q {
            return i + 1;
        }
        i += 1;
    }
    chars.len()
}

/// If a `//` or `/* */` comment starts at `i`, the index after it.
fn skip_comment(chars: &[char], i: usize) -> Option<usize> {
    if chars[i] != '/' {
        return None;
    }
    match chars.get(i + 1) {
        Some('/') => {
            let mut j = i + 2;
            while j < chars.len() && chars[j] != '\n' {
                j += 1;
            }
            Some(j)
        }
        Some('*') => {
            let mut j = i + 2;
            while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
                j += 1;
            }
            Some((j + 2).min(chars.len()))
        }
        _ => None,
    }
}

/// Index of the bracket that closes the one at `open`. Strings and comments
/// are skipped.
fn matching_close(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            i = skip_string(chars, i);
            continue;
        }
        if let Some(end) = skip_comment(chars, i) {
            i = end;
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The text of a quoted string literal, with simple escapes resolved.
fn unquote(chars: &[char]) -> String {
    let closed = chars.len() >= 2 && chars[chars.len() - 1] == chars[0];
    let inner = &chars[1..if closed { chars.len() - 1 } else { chars.len() }];
    let mut out = String::new();
    let mut it = inner.iter();
    while let Some(&c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(&other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Query editor: statements
// ---------------------------------------------------------------------------

/// Split query text into statements: at a top-level `;`, or at a new line
/// when all brackets are closed and the next line does not start with `.`.
/// Comments are replaced by spaces. Empty statements are dropped.
pub fn split_statements(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' || c == '\'' {
            let end = skip_string(&chars, i);
            cur.extend(&chars[i..end]);
            i = end;
            continue;
        }
        if let Some(end) = skip_comment(&chars, i) {
            cur.push(' ');
            i = end;
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        let top = depth <= 0;
        if top && (c == ';' || (c == '\n' && !continues(&chars, i + 1, &cur))) {
            finish(&mut cur, &mut out);
        } else {
            cur.push(c);
        }
        i += 1;
    }
    finish(&mut cur, &mut out);
    out
}

/// True when the statement goes on after a new line: the current text ends
/// with `.`, or the next text (after spaces and comments) starts with `.`.
fn continues(chars: &[char], mut i: usize, cur: &str) -> bool {
    if cur.trim_end().ends_with('.') {
        return true;
    }
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
        } else if let Some(end) = skip_comment(chars, i) {
            i = end;
        } else {
            return chars[i] == '.';
        }
    }
    false
}

fn finish(cur: &mut String, out: &mut Vec<String>) {
    let s = cur.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
    cur.clear();
}

/// One parsed statement of the query editor.
// Statements live only during one run, so the size of `Call` does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    ShowDbs,
    ShowCollections,
    RunCommand(Document),
    Coll { name: String, call: Call },
}

/// A collection method call with its arguments.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Find {
        filter: Document,
        projection: Option<Document>,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    },
    FindOne {
        filter: Document,
        projection: Option<Document>,
    },
    Aggregate(Vec<Document>),
    CountDocuments(Document),
    EstimatedDocumentCount,
    Distinct {
        field: String,
        filter: Document,
    },
    InsertOne(Document),
    InsertMany(Vec<Document>),
    UpdateOne {
        filter: Document,
        update: Update,
        upsert: bool,
    },
    UpdateMany {
        filter: Document,
        update: Update,
        upsert: bool,
    },
    ReplaceOne {
        filter: Document,
        doc: Document,
        upsert: bool,
    },
    DeleteOne(Document),
    DeleteMany(Document),
}

/// An update document (`{$set: …}`) or an aggregation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    Doc(Document),
    Pipeline(Vec<Document>),
}

impl Stmt {
    /// True when the statement can change data.
    pub fn is_write(&self) -> bool {
        match self {
            Stmt::ShowDbs | Stmt::ShowCollections => false,
            Stmt::RunCommand(cmd) => !is_read_command(cmd),
            Stmt::Coll { call, .. } => match call {
                Call::Aggregate(p) => pipeline_writes(p),
                Call::Find { .. }
                | Call::FindOne { .. }
                | Call::CountDocuments(_)
                | Call::EstimatedDocumentCount
                | Call::Distinct { .. } => false,
                _ => true,
            },
        }
    }
}

/// True when an aggregation pipeline writes ($out or $merge).
pub fn pipeline_writes(pipeline: &[Document]) -> bool {
    pipeline
        .iter()
        .any(|s| s.contains_key("$out") || s.contains_key("$merge"))
}

/// Commands that only read. Every other command counts as a write.
fn is_read_command(cmd: &Document) -> bool {
    const READ: [&str; 26] = [
        "ping",
        "buildinfo",
        "hello",
        "ismaster",
        "serverstatus",
        "hostinfo",
        "dbstats",
        "collstats",
        "datasize",
        "listcollections",
        "listdatabases",
        "listindexes",
        "connectionstatus",
        "getparameter",
        "getcmdlineopts",
        "getlog",
        "replsetgetstatus",
        "currentop",
        "count",
        "distinct",
        "find",
        "explain",
        "usersinfo",
        "rolesinfo",
        "whatsmyuri",
        "validate",
    ];
    let Some(name) = cmd.keys().next() else {
        return true;
    };
    let name = name.to_ascii_lowercase();
    if name == "aggregate" {
        return match cmd.get("pipeline") {
            Some(Bson::Array(a)) => !a.iter().any(|s| {
                s.as_document()
                    .is_some_and(|d| d.contains_key("$out") || d.contains_key("$merge"))
            }),
            _ => true,
        };
    }
    READ.contains(&name.as_str())
}

/// One part of a `db.…` expression.
enum Part {
    /// `.name` or `["name"]`
    Name(String),
    /// `(…)` with the text between the parentheses.
    Args(String),
}

/// Parse one statement of the query editor.
pub fn parse_statement(text: &str) -> Result<Stmt> {
    let t = text.trim();
    let mut words = t.split_whitespace();
    match words.next() {
        Some("show") => {
            return match (words.next(), words.next()) {
                (Some("dbs" | "databases"), None) => Ok(Stmt::ShowDbs),
                (Some("collections" | "tables"), None) => Ok(Stmt::ShowCollections),
                _ => bail!(
                    "unknown command `{}`: use `show dbs` or `show collections`",
                    snippet(t)
                ),
            };
        }
        Some("use") => bail!("`use` is not supported: pick the database in the namespace picker"),
        _ => {}
    }
    let parts = parse_parts(t)?;
    interpret(t, parts)
}

/// Split `db.a.b(…).c(…)` into its parts. The text must start with `db`.
fn parse_parts(t: &str) -> Result<Vec<Part>> {
    let chars: Vec<char> = t.chars().collect();
    let shape =
        "expected db.<collection>.<method>(…), db.runCommand({…}), show dbs or show collections";
    if !(t.starts_with("db") && chars.get(2).is_none_or(|c| !is_ident_char(*c))) {
        bail!("{shape}, got `{}`", snippet(t));
    }
    let mut parts = Vec::new();
    let mut i = 2;
    loop {
        i = skip_space(&chars, i);
        if i >= chars.len() {
            break;
        }
        match chars[i] {
            '.' => {
                let s = skip_space(&chars, i + 1);
                if s >= chars.len() || !is_ident_start(chars[s]) {
                    bail!("expected a name after `.` in `{}`", snippet(t));
                }
                let e = ident_end(&chars, s);
                parts.push(Part::Name(chars[s..e].iter().collect()));
                i = e;
            }
            '[' => {
                let close = matching_close(&chars, i)
                    .ok_or_else(|| anyhow!("missing `]` in `{}`", snippet(t)))?;
                let inner: String = chars[i + 1..close].iter().collect();
                match parse_value(&inner)? {
                    Bson::String(s) => parts.push(Part::Name(s)),
                    _ => bail!(
                        "expected a collection name in quotes inside [ ] in `{}`",
                        snippet(t)
                    ),
                }
                i = close + 1;
            }
            '(' => {
                let close = matching_close(&chars, i)
                    .ok_or_else(|| anyhow!("missing `)` in `{}`", snippet(t)))?;
                parts.push(Part::Args(chars[i + 1..close].iter().collect()));
                i = close + 1;
            }
            _ => {
                let rest: String = chars[i..].iter().collect();
                bail!("unexpected text `{}` in `{}`", snippet(&rest), snippet(t));
            }
        }
    }
    Ok(parts)
}

fn interpret(t: &str, parts: Vec<Part>) -> Result<Stmt> {
    let mut it = parts.into_iter().peekable();
    let mut names: Vec<String> = Vec::new();
    // Names up to the first call: the collection path and the method name.
    let mut method: Option<(String, String)> = None;
    while let Some(part) = it.next() {
        match part {
            Part::Name(n) => {
                if matches!(it.peek(), Some(Part::Args(_))) {
                    let Some(Part::Args(a)) = it.next() else {
                        unreachable!()
                    };
                    if names.is_empty() && n == "getCollection" {
                        let args = parse_args(&a)?;
                        match args.as_slice() {
                            [Bson::String(s)] => names.push(s.clone()),
                            _ => bail!("getCollection() expects one collection name in quotes"),
                        }
                        continue;
                    }
                    method = Some((n, a));
                    break;
                }
                names.push(n);
            }
            Part::Args(_) => bail!("unexpected `(` in `{}`", snippet(t)),
        }
    }
    let Some((method, args)) = method else {
        bail!(
            "missing method call, for example db.users.find(), in `{}`",
            snippet(t)
        );
    };
    let args = parse_args(&args).map_err(|e| anyhow!("{method}(): {e}"))?;

    // Chained calls: .sort(…), .limit(…) and so on.
    let mut chain: Vec<(String, Vec<Bson>)> = Vec::new();
    while let Some(part) = it.next() {
        match (part, it.next()) {
            (Part::Name(n), Some(Part::Args(a))) => {
                let a = parse_args(&a).map_err(|e| anyhow!("{n}(): {e}"))?;
                chain.push((n, a));
            }
            (Part::Name(n), _) => bail!("expected `{n}(…)` in `{}`", snippet(t)),
            (Part::Args(_), _) => bail!("unexpected `(` in `{}`", snippet(t)),
        }
    }

    if names.is_empty() {
        if method == "runCommand" {
            no_chain(&method, &chain)?;
            let mut args = Args::new(&method, args);
            let cmd = args.doc("command")?;
            args.done()?;
            return Ok(Stmt::RunCommand(cmd));
        }
        if method == "getCollectionNames" {
            no_chain(&method, &chain)?;
            Args::new(&method, args).done()?;
            return Ok(Stmt::ShowCollections);
        }
        bail!(
            "db.{method}() is not supported; use db.<collection>.<method>(…) or db.runCommand({{…}})"
        );
    }
    let name = names.join(".");
    let call = collection_call(&method, args, chain)?;
    Ok(Stmt::Coll { name, call })
}

fn no_chain(method: &str, chain: &[(String, Vec<Bson>)]) -> Result<()> {
    match chain
        .iter()
        .find(|(n, _)| !matches!(n.as_str(), "pretty" | "toArray"))
    {
        Some((n, _)) => bail!(".{n}() is not supported after {method}()"),
        None => Ok(()),
    }
}

fn collection_call(method: &str, args: Vec<Bson>, chain: Vec<(String, Vec<Bson>)>) -> Result<Call> {
    let mut a = Args::new(method, args);
    let call = match method {
        "find" => {
            let filter = a.opt_doc("filter")?.unwrap_or_default();
            let projection = a.opt_doc("projection")?;
            a.done()?;
            let (mut sort, mut skip, mut limit, mut projection) = (None, None, None, projection);
            for (n, args) in chain {
                let mut c = Args::new(&n, args);
                match n.as_str() {
                    "sort" => sort = Some(c.doc("sort")?),
                    "skip" => {
                        skip = Some(
                            u64::try_from(c.int("skip")?)
                                .map_err(|_| anyhow!("skip() must not be negative"))?,
                        )
                    }
                    "limit" => limit = Some(c.int("limit")?),
                    "projection" => projection = Some(c.doc("projection")?),
                    "pretty" | "toArray" => {}
                    _ => bail!(
                        ".{n}() is not supported after find(); use sort, skip, limit or projection"
                    ),
                }
                c.done()?;
            }
            return Ok(Call::Find {
                filter,
                projection,
                sort,
                skip,
                limit,
            });
        }
        "findOne" => Call::FindOne {
            filter: a.opt_doc("filter")?.unwrap_or_default(),
            projection: a.opt_doc("projection")?,
        },
        "aggregate" => Call::Aggregate(a.doc_list("pipeline")?),
        "countDocuments" => Call::CountDocuments(a.opt_doc("filter")?.unwrap_or_default()),
        "estimatedDocumentCount" => Call::EstimatedDocumentCount,
        "distinct" => Call::Distinct {
            field: a.string("field")?,
            filter: a.opt_doc("filter")?.unwrap_or_default(),
        },
        "insertOne" => Call::InsertOne(a.doc("document")?),
        "insertMany" => Call::InsertMany(a.doc_list("documents")?),
        "updateOne" | "updateMany" => {
            let filter = a.doc("filter")?;
            let update = match a.next("update")? {
                Bson::Document(d) => Update::Doc(d),
                Bson::Array(p) => Update::Pipeline(docs_of(p, "update pipeline")?),
                _ => bail!(
                    "{method}(): the update must be a document like {{$set: {{…}}}} or a pipeline"
                ),
            };
            let upsert = a.upsert()?;
            if method == "updateOne" {
                Call::UpdateOne {
                    filter,
                    update,
                    upsert,
                }
            } else {
                Call::UpdateMany {
                    filter,
                    update,
                    upsert,
                }
            }
        }
        "replaceOne" => {
            let filter = a.doc("filter")?;
            let doc = a.doc("replacement")?;
            Call::ReplaceOne {
                filter,
                doc,
                upsert: a.upsert()?,
            }
        }
        "deleteOne" => Call::DeleteOne(a.doc("filter")?),
        "deleteMany" => Call::DeleteMany(a.doc("filter")?),
        _ => bail!(
            "{method}() is not supported; use find, findOne, aggregate, countDocuments, estimatedDocumentCount, distinct, insertOne, insertMany, updateOne, updateMany, replaceOne, deleteOne or deleteMany"
        ),
    };
    a.done()?;
    no_chain(method, &chain)?;
    Ok(call)
}

fn docs_of(list: Vec<Bson>, what: &str) -> Result<Vec<Document>> {
    list.into_iter()
        .map(|b| match b {
            Bson::Document(d) => Ok(d),
            other => bail!("{what}: expected documents, got {}", type_name(&other)),
        })
        .collect()
}

/// Reads the arguments of one call in order.
struct Args<'a> {
    method: &'a str,
    list: std::vec::IntoIter<Bson>,
}

impl<'a> Args<'a> {
    fn new(method: &'a str, list: Vec<Bson>) -> Self {
        Args {
            method,
            list: list.into_iter(),
        }
    }

    fn next(&mut self, what: &str) -> Result<Bson> {
        self.list
            .next()
            .ok_or_else(|| anyhow!("{}(): missing argument `{what}`", self.method))
    }

    fn doc(&mut self, what: &str) -> Result<Document> {
        match self.next(what)? {
            Bson::Document(d) => Ok(d),
            other => bail!(
                "{}(): `{what}` must be a document, got {}",
                self.method,
                type_name(&other)
            ),
        }
    }

    fn opt_doc(&mut self, what: &str) -> Result<Option<Document>> {
        match self.list.next() {
            None => Ok(None),
            Some(Bson::Document(d)) => Ok(Some(d)),
            Some(other) => bail!(
                "{}(): `{what}` must be a document, got {}",
                self.method,
                type_name(&other)
            ),
        }
    }

    fn doc_list(&mut self, what: &str) -> Result<Vec<Document>> {
        match self.next(what)? {
            Bson::Array(a) => docs_of(a, &format!("{}()", self.method)),
            other => bail!(
                "{}(): `{what}` must be an array of documents, got {}",
                self.method,
                type_name(&other)
            ),
        }
    }

    fn string(&mut self, what: &str) -> Result<String> {
        match self.next(what)? {
            Bson::String(s) => Ok(s),
            other => bail!(
                "{}(): `{what}` must be a string, got {}",
                self.method,
                type_name(&other)
            ),
        }
    }

    fn int(&mut self, what: &str) -> Result<i64> {
        match self.next(what)? {
            Bson::Int32(n) => Ok(n.into()),
            Bson::Int64(n) => Ok(n),
            Bson::Double(f) if f.fract() == 0.0 && f.abs() < 9.0e18 => Ok(f as i64),
            other => bail!(
                "{}(): `{what}` must be an integer, got {}",
                self.method,
                type_name(&other)
            ),
        }
    }

    /// The options document of an update: only `upsert` is supported.
    fn upsert(&mut self) -> Result<bool> {
        let Some(opts) = self.opt_doc("options")? else {
            return Ok(false);
        };
        let mut upsert = false;
        for (k, v) in &opts {
            match (k.as_str(), v) {
                ("upsert", Bson::Boolean(b)) => upsert = *b,
                ("upsert", _) => bail!("{}(): upsert must be true or false", self.method),
                _ => bail!(
                    "{}(): option `{k}` is not supported (only upsert)",
                    self.method
                ),
            }
        }
        Ok(upsert)
    }

    /// Fail when there are more arguments than the method takes.
    fn done(&mut self) -> Result<()> {
        if self.list.next().is_some() {
            bail!("{}(): too many arguments", self.method);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    fn keys(d: &Document) -> Vec<&str> {
        d.keys().map(String::as_str).collect()
    }

    #[test]
    fn json5_and_helpers() {
        let d = parse_doc(r#"{_id: ObjectId("64b7f0c2a1b2c3d4e5f60718"), name: 'Ann', n: NumberLong(5), i: NumberInt("7"),
            d: NumberDecimal("1.50"), at: ISODate("2024-05-01T10:00:00Z"), at2: new Date('2024-05-01'), ms: new Date(0), trailing: [1, 2,],}"#)
        .unwrap();
        assert_eq!(
            d.get("_id"),
            Some(&Bson::ObjectId(
                ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").unwrap()
            ))
        );
        assert_eq!(d.get("name"), Some(&Bson::String("Ann".into())));
        assert_eq!(d.get("n"), Some(&Bson::Int64(5)));
        assert_eq!(d.get("i"), Some(&Bson::Int32(7)));
        assert_eq!(d.get("d"), Some(&Bson::Decimal128("1.50".parse().unwrap())));
        assert_eq!(
            d.get("at"),
            Some(&Bson::DateTime(DateTime::from_millis(1_714_557_600_000)))
        );
        assert_eq!(
            d.get("at2"),
            Some(&Bson::DateTime(DateTime::from_millis(1_714_521_600_000)))
        );
        assert_eq!(d.get("ms"), Some(&Bson::DateTime(DateTime::from_millis(0))));
        assert_eq!(
            d.get("trailing"),
            Some(&Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)]))
        );
        assert_eq!(
            keys(&d),
            ["_id", "name", "n", "i", "d", "at", "at2", "ms", "trailing"]
        );
    }

    #[test]
    fn numbers_and_order() {
        let d = parse_doc("{z: 1, a: 2.0, m: 3000000000, neg: -4, big: 1e300}").unwrap();
        assert_eq!(keys(&d), ["z", "a", "m", "neg", "big"]);
        assert_eq!(d.get("z"), Some(&Bson::Int32(1)));
        assert_eq!(d.get("a"), Some(&Bson::Double(2.0)));
        assert_eq!(d.get("m"), Some(&Bson::Int64(3_000_000_000)));
        assert_eq!(d.get("neg"), Some(&Bson::Int32(-4)));
        assert_eq!(d.get("big"), Some(&Bson::Double(1e300)));
        // Nested order and operators.
        let d = parse_doc("{$or: [{b: {$gt: 1}}, {a: {$regex: '^x', $options: 'i'}}]}").unwrap();
        assert_eq!(
            d,
            doc! {"$or": [{"b": {"$gt": 1}}, {"a": {"$regex": "^x", "$options": "i"}}]}
        );
    }

    #[test]
    fn nan_and_infinity_stay_doubles() {
        let d = parse_doc("{a: NaN, b: Infinity, c: - Infinity, NaN: 1}").unwrap();
        assert!(d.get_f64("a").unwrap().is_nan());
        assert_eq!(d.get("b"), Some(&Bson::Double(f64::INFINITY)));
        assert_eq!(d.get("c"), Some(&Bson::Double(f64::NEG_INFINITY)));
        assert_eq!(d.get("NaN"), Some(&Bson::Int32(1)));
    }

    #[test]
    fn strings_and_comments_are_not_rewritten() {
        let d =
            parse_doc("{s: 'ObjectId(\"x\")', t: \"new Date(1)\", // ObjectId(bad)\n ObjectId: 1}")
                .unwrap();
        assert_eq!(
            d,
            doc! {"s": "ObjectId(\"x\")", "t": "new Date(1)", "ObjectId": 1}
        );
    }

    #[test]
    fn dates() {
        let ms = |s: &str| parse_date(s).unwrap().timestamp_millis();
        assert_eq!(ms("1970-01-01"), 0);
        assert_eq!(ms("1970-01-01T00:00:01"), 1000);
        assert_eq!(ms("1970-01-01 00:01"), 60_000);
        assert_eq!(ms("1970-01-01T09:00:00+09:00"), 0);
        assert_eq!(ms("1970-01-01T09:00:00+0900"), 0);
        assert_eq!(ms("1970-01-01T00:00:00.123Z"), 123);
        assert!(parse_date("yesterday").is_err());
        assert_eq!(parse_millis("1700000000000"), Some(1_700_000_000_000));
    }

    #[test]
    fn errors_show_the_bad_text() {
        let e = parse_doc("{a: 1 b: 2}").unwrap_err().to_string();
        assert!(e.contains("syntax error") && e.contains("b: 2}"), "{e}");
        let e = parse_doc("{_id: ObjectId('xyz')}").unwrap_err().to_string();
        assert!(e.contains("ObjectId('xyz')") && e.contains("24 hex"), "{e}");
        let e = parse_doc("{n: NumberLong('abc')}").unwrap_err().to_string();
        assert!(e.contains("NumberLong('abc')"), "{e}");
        let e = parse_doc("{d: ISODate('soon')}").unwrap_err().to_string();
        assert!(e.contains("ISODate('soon')"), "{e}");
        let e = parse_doc("[1, 2]").unwrap_err().to_string();
        assert!(e.contains("expected a document"), "{e}");
        let e = parse_doc("{a: {$oid: 'nothex'}}").unwrap_err().to_string();
        assert!(
            e.starts_with("invalid value in `{a: {$oid: 'nothex'}}`"),
            "{e}"
        );
    }

    #[test]
    fn split() {
        let text = "db.a.find({x: ';'})\n  .sort({x: 1})\n  // note\n  .limit(2); db.b.countDocuments()\n\n\
                    db.c.insertOne({\n  a: 1\n})\n// done\nshow dbs";
        assert_eq!(
            split_statements(text),
            vec![
                "db.a.find({x: ';'})\n  .sort({x: 1})\n   \n  .limit(2)",
                "db.b.countDocuments()",
                "db.c.insertOne({\n  a: 1\n})",
                "show dbs",
            ]
        );
        assert_eq!(split_statements("db.a.\nfind()"), vec!["db.a.\nfind()"]);
        assert!(split_statements(" ;\n // only a comment\n").is_empty());
    }

    #[test]
    fn statements() {
        let s = parse_statement("db.users.find({age: {$gt: 30}}, {name: 1})\n .sort({age: -1, name: 1}).skip(5).limit(10)").unwrap();
        let Stmt::Coll {
            name,
            call:
                Call::Find {
                    filter,
                    projection,
                    sort,
                    skip,
                    limit,
                },
        } = s
        else {
            panic!("{s:?}")
        };
        assert_eq!(name, "users");
        assert_eq!(filter, doc! {"age": {"$gt": 30}});
        assert_eq!(projection, Some(doc! {"name": 1}));
        assert_eq!(keys(sort.as_ref().unwrap()), ["age", "name"]);
        assert_eq!((skip, limit), (Some(5), Some(10)));

        let s = parse_statement("db.getCollection('my-coll').countDocuments()").unwrap();
        assert_eq!(
            s,
            Stmt::Coll {
                name: "my-coll".into(),
                call: Call::CountDocuments(Document::new())
            }
        );
        let s = parse_statement("db['a b'].estimatedDocumentCount()").unwrap();
        assert_eq!(
            s,
            Stmt::Coll {
                name: "a b".into(),
                call: Call::EstimatedDocumentCount
            }
        );
        let s = parse_statement("db.system.profile.find()").unwrap();
        assert!(matches!(s, Stmt::Coll { ref name, .. } if name == "system.profile"));
        let s = parse_statement("db.runCommand({find: 'x', filter: {}})").unwrap();
        let Stmt::RunCommand(cmd) = &s else { panic!() };
        assert_eq!(keys(cmd), ["find", "filter"]);
        assert!(!s.is_write());
        assert!(
            parse_statement("db.runCommand({drop: 'x'})")
                .unwrap()
                .is_write()
        );
        assert_eq!(
            parse_statement("show  collections").unwrap(),
            Stmt::ShowCollections
        );
        assert_eq!(parse_statement("show databases").unwrap(), Stmt::ShowDbs);

        let s = parse_statement("db.t.updateMany({}, [{$set: {a: 1}}], {upsert: true})").unwrap();
        assert!(matches!(
            s,
            Stmt::Coll {
                call: Call::UpdateMany {
                    update: Update::Pipeline(_),
                    upsert: true,
                    ..
                },
                ..
            }
        ));
        assert!(s.is_write());
        let s = parse_statement("db.t.distinct('tag', {a: 1})").unwrap();
        assert_eq!(
            s,
            Stmt::Coll {
                name: "t".into(),
                call: Call::Distinct {
                    field: "tag".into(),
                    filter: doc! {"a": 1}
                }
            }
        );
        assert!(
            parse_statement("db.t.aggregate([{$match: {}}, {$out: 'x'}])")
                .unwrap()
                .is_write()
        );
        assert!(
            !parse_statement("db.t.aggregate([{$match: {}}]).toArray()")
                .unwrap()
                .is_write()
        );
    }

    #[test]
    fn statement_errors() {
        let err = |s: &str| parse_statement(s).unwrap_err().to_string();
        assert!(
            err("select 1").contains("expected db."),
            "{}",
            err("select 1")
        );
        assert!(err("db.users").contains("missing method"));
        assert!(
            err("db.users.find({a: })").starts_with("find(): syntax error"),
            "{}",
            err("db.users.find({a: })")
        );
        assert!(err("db.users.findOneAndDelete({})").contains("not supported"));
        assert!(err("db.users.find().count()").contains(".count() is not supported"));
        assert!(err("db.users.find(1)").contains("must be a document"));
        assert!(err("db.users.insertOne({}, {}, {})").contains("too many arguments"));
        assert!(err("db.users.find(").contains("missing `)`"));
        assert!(err("use test").contains("namespace picker"));
        assert!(err("db.users.updateOne({}, {$set: {a: 1}}, {multi: true})").contains("multi"));
    }
}
