//! OurDB driver: SQLite (rusqlite with bundled SQLite).
//!
//! The driver keeps two connections to the same file. The `meta` connection
//! serves the sidebar, browse, count and grid edits. The `editor` connection
//! serves the query editor, so its session state (temp tables, BEGIN, ATTACH)
//! stays between runs. All database work runs in `spawn_blocking`.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use ourdb_core::{
    BrowseRequest, Change, ColumnInfo, ConnConfig, Driver, Edit, Kind, ObjectInfo, ObjectKind,
    ObjectRef, Page, ResultSet, Value,
    sql::{qualified, quote_ident, sqlite_literal, x_literal},
};
use rusqlite::{
    Batch, Connection, InterruptHandle, OpenFlags, Row, TransactionBehavior,
    fallible_iterator::FallibleIterator, types::ValueRef,
};

/// How long a connection waits for a lock that another connection holds.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type Shared = Arc<Mutex<Connection>>;

/// Open a connection. The password is already in `cfg.password`.
pub async fn connect(cfg: &ConnConfig) -> anyhow::Result<Arc<dyn Driver>> {
    let path = expand_home(cfg.path.trim());
    if path.as_os_str().is_empty() {
        bail!("no SQLite file is set");
    }
    if !path.exists() {
        bail!("file not found: {}", path.display());
    }
    let read_only = cfg.read_only;
    let (meta, editor) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok((open(&path, read_only)?, open(&path, read_only)?))
    })
    .await
    .map_err(|e| anyhow!("open task failed: {e}"))??;

    let interrupt = editor.get_interrupt_handle();
    Ok(Arc::new(Sqlite {
        meta: Arc::new(Mutex::new(meta)),
        editor: Arc::new(Mutex::new(editor)),
        interrupt,
        cancelled: Arc::new(AtomicBool::new(false)),
        read_only,
    }))
}

/// Replace a leading `~/` with the home folder.
fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

/// Open the file without CREATE, so a typo never makes a new file.
fn open(path: &std::path::Path, read_only: bool) -> anyhow::Result<Connection> {
    let mode = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(path, mode | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .with_context(|| format!("cannot open {}", path.display()))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

struct Sqlite {
    meta: Shared,
    editor: Shared,
    /// Interrupts the statement that runs on the editor connection.
    interrupt: InterruptHandle,
    /// Set by `cancel()`. `execute` checks it between statements, because an
    /// interrupt has no effect when no statement runs at that moment.
    cancelled: Arc<AtomicBool>,
    read_only: bool,
}

/// Run `f` with the connection on a blocking thread.
async fn run<T, F>(conn: &Shared, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
{
    let conn = conn.clone();
    tokio::task::spawn_blocking(move || {
        // A panic in an earlier task must not lock the connection forever.
        let mut guard = conn.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    })
    .await
    .map_err(|e| anyhow!("database task failed: {e}"))?
}

#[async_trait::async_trait]
impl Driver for Sqlite {
    fn kind(&self) -> Kind {
        Kind::Sqlite
    }

    async fn namespaces(&self) -> anyhow::Result<Vec<String>> {
        run(&self.meta, |c| {
            let mut stmt = c.prepare("SELECT name FROM pragma_database_list ORDER BY seq")?;
            let names = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(names.collect::<Result<_, _>>()?)
        })
        .await
    }

    fn default_namespace(&self) -> Option<String> {
        Some("main".into())
    }

    async fn objects(&self, namespace: &str) -> anyhow::Result<Vec<ObjectInfo>> {
        let sql = format!(
            "SELECT name, type FROM {}.sqlite_master \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
            quote_ident(Kind::Sqlite, namespace)
        );
        run(&self.meta, move |c| {
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map([], |r| {
                let kind = if r.get::<_, String>(1)? == "view" {
                    ObjectKind::View
                } else {
                    ObjectKind::Table
                };
                Ok(ObjectInfo {
                    name: r.get(0)?,
                    kind,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    async fn columns(&self, object: &ObjectRef) -> anyhow::Result<Vec<ColumnInfo>> {
        let object = object.clone();
        run(&self.meta, move |c| Ok(table_meta(c, &object)?.columns)).await
    }

    async fn browse(&self, req: &BrowseRequest) -> anyhow::Result<Page> {
        let req = req.clone();
        let read_only = self.read_only;
        run(&self.meta, move |c| {
            let meta = table_meta(c, &req.object)?;
            let mut sql = format!("SELECT * FROM {}", target(&req.object));
            push_filter(&mut sql, &req.filter);
            match &req.sort {
                Some((col, asc)) => {
                    let dir = if *asc { "ASC" } else { "DESC" };
                    sql.push_str(&format!(
                        " ORDER BY {} {dir}",
                        quote_ident(Kind::Sqlite, col)
                    ));
                }
                // Order by the key, so pages are stable.
                None if !meta.key.is_empty() => {
                    let cols: Vec<String> = meta
                        .key
                        .iter()
                        .map(|k| quote_ident(Kind::Sqlite, k))
                        .collect();
                    sql.push_str(&format!(" ORDER BY {}", cols.join(", ")));
                }
                None => {}
            }
            sql.push_str(&format!(" LIMIT {} OFFSET {}", req.limit, req.offset));

            let start = Instant::now();
            let mut stmt = c.prepare(&sql)?;
            let columns: Vec<String> = stmt.column_names().map(String::from).collect();
            let types = columns
                .iter()
                .map(|name| {
                    let col = meta.columns.iter().find(|ci| &ci.name == name);
                    col.map(|ci| ci.type_name.clone()).unwrap_or_default()
                })
                .collect();
            let mut rows = Vec::new();
            let mut cursor = stmt.query([])?;
            while let Some(row) = cursor.next()? {
                rows.push(read_row(row, columns.len())?);
            }

            let reason = if meta.is_view {
                Some("view")
            } else if read_only {
                Some("read-only connection")
            } else if meta.key.is_empty() {
                Some("no primary key")
            } else {
                None
            };
            Ok(Page {
                result: ResultSet {
                    columns,
                    types,
                    rows,
                    elapsed: start.elapsed(),
                    ..Default::default()
                },
                key: if reason.is_none() {
                    meta.key
                } else {
                    Vec::new()
                },
                read_only_reason: reason.map(String::from),
            })
        })
        .await
    }

    async fn count(&self, req: &BrowseRequest) -> anyhow::Result<u64> {
        let mut sql = format!("SELECT count(*) FROM {}", target(&req.object));
        push_filter(&mut sql, &req.filter);
        run(&self.meta, move |c| {
            let n: i64 = c.query_row(&sql, [], |r| r.get(0))?;
            Ok(n as u64)
        })
        .await
    }

    /// SQLite has no USE, so `namespace` is ignored. Attached databases are
    /// reachable as `name.table`.
    async fn execute(
        &self,
        _namespace: Option<&str>,
        text: &str,
        max_rows: usize,
    ) -> anyhow::Result<Vec<ResultSet>> {
        let text = text.to_string();
        let cancelled = self.cancelled.clone();
        run(&self.editor, move |c| {
            let c: &Connection = c;
            cancelled.store(false, Ordering::SeqCst);
            let mut out = Vec::new();
            let mut batch = Batch::new(c, &text);
            let mut n = 0;
            loop {
                n += 1;
                if cancelled.load(Ordering::SeqCst) {
                    bail!("statement {n}: interrupted");
                }
                let start = Instant::now();
                let stmt = match batch.next() {
                    Ok(Some(stmt)) => stmt,
                    Ok(None) => break,
                    Err(e) => bail!("statement {n}: {e}"),
                };
                let mut rs =
                    run_statement(c, stmt, max_rows).map_err(|e| anyhow!("statement {n}: {e}"))?;
                rs.elapsed = start.elapsed();
                out.push(rs);
            }
            Ok(out)
        })
        .await
    }

    async fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.interrupt.interrupt();
    }

    fn preview(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<String>> {
        Ok(statements(object, changes)?
            .into_iter()
            .map(|s| s.sql)
            .collect())
    }

    async fn apply(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<u64> {
        let stmts = statements(object, changes)?;
        let count = stmts.len() as u64;
        run(&self.meta, move |c| {
            // The transaction rolls back when it is dropped without commit.
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (i, s) in stmts.iter().enumerate() {
                let n = i + 1;
                let changed = tx.execute(&s.sql, []).map_err(|e| anyhow!("statement {n}: {e}"))?;
                if s.one_row && changed != 1 {
                    bail!("statement {n} changed {changed} rows, expected exactly 1. Nothing was saved.\n{}", s.sql);
                }
            }
            tx.commit()?;
            Ok(count)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

struct TableMeta {
    is_view: bool,
    columns: Vec<ColumnInfo>,
    /// Primary key columns in key order.
    key: Vec<String>,
}

/// Columns, primary key and kind of a table or view.
fn table_meta(c: &Connection, object: &ObjectRef) -> anyhow::Result<TableMeta> {
    // table_xinfo also lists generated columns, which `SELECT *` returns.
    // `hidden = 1` marks hidden columns of virtual tables; skip them.
    let mut stmt = c.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_xinfo(?1, ?2) \
         WHERE hidden <> 1 ORDER BY cid",
    )?;
    let rows = stmt.query_map([&object.name, &object.namespace], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, bool>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut columns = Vec::new();
    let mut pk = Vec::new();
    for row in rows {
        let (name, type_name, not_null, default, pk_pos) = row?;
        if pk_pos > 0 {
            pk.push((pk_pos, name.clone()));
        }
        columns.push(ColumnInfo {
            name,
            type_name,
            nullable: !not_null,
            key: pk_pos > 0,
            default,
        });
    }
    if columns.is_empty() {
        bail!("no such table: {}.{}", object.namespace, object.name);
    }
    pk.sort();

    let master = format!(
        "SELECT type FROM {}.sqlite_master WHERE name = ?1",
        quote_ident(Kind::Sqlite, &object.namespace)
    );
    let kind: Option<String> = c.query_row(&master, [&object.name], |r| r.get(0)).ok();
    Ok(TableMeta {
        is_view: kind.as_deref() == Some("view"),
        columns,
        key: pk.into_iter().map(|(_, name)| name).collect(),
    })
}

/// `"ns"."table"`.
fn target(object: &ObjectRef) -> String {
    qualified(Kind::Sqlite, &object.namespace, &object.name)
}

/// Add ` WHERE (filter)`. The filter sits on its own lines, so a `--`
/// comment in it cannot hide the closing parenthesis.
fn push_filter(sql: &mut String, filter: &str) {
    if !filter.trim().is_empty() {
        sql.push_str(&format!(" WHERE (\n{filter}\n)"));
    }
}

// ---------------------------------------------------------------------------
// Rows and execute
// ---------------------------------------------------------------------------

fn to_value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Int(i),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Bytes(b.to_vec()),
    }
}

fn read_row(row: &Row<'_>, width: usize) -> rusqlite::Result<Vec<Value>> {
    (0..width).map(|i| row.get_ref(i).map(to_value)).collect()
}

/// Run one statement of a batch. Statements with columns give rows (at most
/// `max_rows`); other statements give the number of changed rows.
fn run_statement(
    c: &Connection,
    mut stmt: rusqlite::Statement<'_>,
    max_rows: usize,
) -> rusqlite::Result<ResultSet> {
    let width = stmt.column_count();
    if width == 0 {
        // changes() keeps the count of the last INSERT/UPDATE/DELETE, so use it
        // only when this statement changed rows.
        let before = c.total_changes();
        stmt.execute([])?;
        let n = if c.total_changes() != before {
            c.changes()
        } else {
            0
        };
        let message = format!("{n} row{} affected", if n == 1 { "" } else { "s" });
        return Ok(ResultSet {
            affected: Some(n),
            message: Some(message),
            ..Default::default()
        });
    }

    let columns: Vec<String> = stmt.column_names().map(String::from).collect();
    let types: Vec<String> = stmt
        .columns()
        .map(|col| col.decl_type().unwrap_or_default().to_string())
        .collect();
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut cursor = stmt.query([])?;
    while let Some(row) = cursor.next()? {
        // Read one row past the cap to know that more rows exist.
        if rows.len() == max_rows {
            truncated = true;
            break;
        }
        rows.push(read_row(row, width)?);
    }
    Ok(ResultSet {
        columns,
        types,
        rows,
        truncated,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

/// One statement of a grid save.
struct Stmt {
    sql: String,
    /// UPDATE and DELETE must change exactly one row.
    one_row: bool,
}

/// Build the statements for `changes`. `preview` and `apply` both use this,
/// so the preview text is exactly what runs.
fn statements(object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<Stmt>> {
    let table = target(object);
    let mut out = Vec::new();
    for change in changes {
        match change {
            Change::Update { key, set } => {
                if set.is_empty() {
                    continue;
                }
                let assign: Vec<String> = set
                    .iter()
                    .map(|(col, orig, edit)| {
                        Ok(format!(
                            "{} = {}",
                            quote_ident(Kind::Sqlite, col),
                            edit_literal(col, Some(orig), edit)?
                        ))
                    })
                    .collect::<anyhow::Result<_>>()?;
                let sql = format!(
                    "UPDATE {table} SET {} WHERE {}",
                    assign.join(", "),
                    where_key(key)?
                );
                out.push(Stmt { sql, one_row: true });
            }
            Change::Insert { values } => {
                let sql = if values.is_empty() {
                    format!("INSERT INTO {table} DEFAULT VALUES")
                } else {
                    let cols: Vec<String> = values
                        .iter()
                        .map(|(col, _)| quote_ident(Kind::Sqlite, col))
                        .collect();
                    let vals: Vec<String> = values
                        .iter()
                        .map(|(col, edit)| edit_literal(col, None, edit))
                        .collect::<anyhow::Result<_>>()?;
                    format!(
                        "INSERT INTO {table} ({}) VALUES ({})",
                        cols.join(", "),
                        vals.join(", ")
                    )
                };
                out.push(Stmt {
                    sql,
                    one_row: false,
                });
            }
            Change::Delete { key } => {
                let sql = format!("DELETE FROM {table} WHERE {}", where_key(key)?);
                out.push(Stmt { sql, one_row: true });
            }
            Change::InsertDoc { .. } | Change::ReplaceDoc { .. } => {
                bail!("document changes are not supported for SQLite")
            }
        }
    }
    Ok(out)
}

/// `"a" = 1 AND "b" IS NULL`. An empty key would change every row, so refuse it.
fn where_key(key: &[(String, Value)]) -> anyhow::Result<String> {
    if key.is_empty() {
        bail!("the table has no primary key, so rows cannot be changed");
    }
    let parts: Vec<String> = key
        .iter()
        .map(|(col, v)| {
            let col = quote_ident(Kind::Sqlite, col);
            Ok(match v {
                Value::Null => format!("{col} IS NULL"),
                v => format!("{col} = {}", value_literal(v)?),
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(parts.join(" AND "))
}

/// SQL literal for a value read from the database.
fn value_literal(v: &Value) -> anyhow::Result<String> {
    Ok(match v {
        Value::Null => "NULL".into(),
        Value::Int(i) => i.to_string(),
        // Debug prints the shortest text that reads back to the same f64.
        Value::Float(f) if f.is_finite() => format!("{f:?}"),
        Value::Float(f) if f.is_nan() => "NULL".into(),
        Value::Float(f) => if *f > 0.0 { "9e999" } else { "-9e999" }.into(),
        Value::Bool(b) => i32::from(*b).to_string(),
        Value::Text(s) => sqlite_literal(s),
        Value::Bytes(b) => x_literal(b),
        Value::Missing | Value::Json(_) => bail!("unsupported key value: {v:?}"),
    })
}

/// SQL literal for user input. `orig` is the value before the edit (None for
/// a new row). A number stays unquoted when the original was a number, and
/// `0x…` becomes a blob when the original was a blob.
fn edit_literal(col: &str, orig: Option<&Value>, edit: &Edit) -> anyhow::Result<String> {
    let s = match edit {
        Edit::Null => return Ok("NULL".into()),
        Edit::Text(s) => s,
    };
    match orig {
        Some(Value::Int(_) | Value::Float(_)) if is_number(s) => Ok(s.trim().to_string()),
        Some(Value::Bytes(_)) if s.starts_with("0x") => {
            let bytes =
                parse_hex(&s[2..]).with_context(|| format!("column {col}: invalid hex value"))?;
            Ok(x_literal(&bytes))
        }
        _ => Ok(sqlite_literal(s)),
    }
}

/// True for a plain decimal number such as `42`, `-1.5` or `2e10`.
fn is_number(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
        && t.parse::<f64>().is_ok()
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        let int = Value::Int(1);
        let blob = Value::Bytes(vec![1]);
        let text = |s: &str| Edit::Text(s.into());
        assert_eq!(edit_literal("c", Some(&int), &text("42")).unwrap(), "42");
        assert_eq!(
            edit_literal("c", Some(&int), &text(" -1.5e3 ")).unwrap(),
            "-1.5e3"
        );
        assert_eq!(
            edit_literal("c", Some(&int), &text("--5")).unwrap(),
            "'--5'"
        );
        assert_eq!(
            edit_literal("c", Some(&int), &text("inf")).unwrap(),
            "'inf'"
        );
        assert_eq!(
            edit_literal("c", Some(&Value::Text("1".into())), &text("42")).unwrap(),
            "'42'"
        );
        assert_eq!(edit_literal("c", None, &text("42")).unwrap(), "'42'");
        assert_eq!(
            edit_literal("c", Some(&blob), &text("0xDEad")).unwrap(),
            "X'dead'"
        );
        assert_eq!(edit_literal("c", Some(&blob), &text("0x")).unwrap(), "X''");
        assert!(edit_literal("c", Some(&blob), &text("0xabc")).is_err());
        assert_eq!(
            edit_literal("c", Some(&blob), &text("abc")).unwrap(),
            "'abc'"
        );
        assert_eq!(edit_literal("c", Some(&int), &Edit::Null).unwrap(), "NULL");
        assert_eq!(value_literal(&Value::Float(0.1)).unwrap(), "0.1");
        assert_eq!(value_literal(&Value::Float(1e300)).unwrap(), "1e300");
    }

    #[test]
    fn home() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_home("~/a.db"), PathBuf::from(home).join("a.db"));
        assert_eq!(expand_home("/x/~/a.db"), PathBuf::from("/x/~/a.db"));
    }
}
