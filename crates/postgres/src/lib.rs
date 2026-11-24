//! OurDB driver for PostgreSQL (tokio-postgres).
//!
//! The driver keeps two sessions. The meta session serves the sidebar, browse,
//! count and apply. The editor session serves the query editor, so its state
//! (SET, temp tables, BEGIN) persists between runs. Browse and execute use the
//! simple query protocol: every value comes as text or NULL, as psql shows it.

mod conn;
mod sql;

use std::{
    collections::HashMap,
    pin::pin,
    sync::{
        Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{anyhow, bail};
use futures_util::TryStreamExt;
use ourdb_core::{
    BrowseRequest, Change, ColumnInfo, ConnConfig, Driver, Kind, ObjectInfo, ObjectKind, ObjectRef,
    Page, ResultSet, Value,
    sql::{pg_literal, qualified, quote_ident, split_statements},
};
use tokio::sync::{Mutex, MutexGuard};
use tokio_postgres::{CancelToken, Client, SimpleQueryMessage, SimpleQueryRow, error::SqlState};

use conn::{Opts, error_text, error_text_with, line_col};

/// Open a connection. The password is already in `cfg.password`.
pub async fn connect(cfg: &ConnConfig) -> anyhow::Result<Arc<dyn Driver>> {
    let opts = Opts::new(cfg)?;
    let (meta, editor) = tokio::try_join!(opts.open(), opts.open())?;
    let std_strings = read_std_strings(&meta).await?;
    Ok(Arc::new(PgDriver {
        read_only: cfg.read_only,
        std_strings: AtomicBool::new(std_strings),
        editor_cancel: StdMutex::new(editor.cancel_token()),
        meta: Mutex::new(meta),
        editor: Mutex::new(editor),
        types: StdMutex::default(),
        opts,
    }))
}

struct PgDriver {
    opts: Opts,
    read_only: bool,
    /// Session for metadata, browse, count and apply.
    meta: Mutex<Client>,
    /// Session for the query editor.
    editor: Mutex<Client>,
    /// Cancel token of the editor session. It is not inside `editor`,
    /// because `execute` holds that lock while the query runs.
    editor_cancel: StdMutex<CancelToken>,
    /// `standard_conforming_strings` of the meta session.
    std_strings: AtomicBool,
    /// Column types per table, from the last `columns` or `browse` call.
    /// `preview` cannot ask the server, so it uses this.
    types: StdMutex<HashMap<ObjectRef, HashMap<String, String>>>,
}

/// Columns, kind and primary key of a table, from the catalog.
struct TableInfo {
    columns: Vec<ColumnInfo>,
    /// `pg_class.relkind`: "r", "p", "f", "v" or "m".
    relkind: String,
    /// Primary key columns in index order.
    pk: Vec<String>,
}

const NAMESPACES_SQL: &str = "SELECT nspname FROM pg_namespace \
     WHERE left(nspname, 3) <> 'pg_' AND nspname <> 'information_schema' \
     ORDER BY nspname";

const DATABASES_SQL: &str =
    "SELECT datname FROM pg_database WHERE NOT datistemplate ORDER BY datname";

impl PgDriver {
    /// The meta session. A closed session is opened again.
    async fn meta(&self) -> anyhow::Result<MutexGuard<'_, Client>> {
        let mut client = self.meta.lock().await;
        if client.is_closed() {
            *client = self
                .opts
                .open()
                .await
                .map_err(|e| anyhow!("Cannot reconnect: {e}"))?;
            self.std_strings
                .store(read_std_strings(&client).await?, Ordering::Relaxed);
        }
        Ok(client)
    }

    /// The editor session. A closed session is opened again (its SET values,
    /// temp tables and open transaction are gone).
    async fn editor(&self) -> anyhow::Result<MutexGuard<'_, Client>> {
        let mut client = self.editor.lock().await;
        if client.is_closed() {
            *client = self
                .opts
                .open()
                .await
                .map_err(|e| anyhow!("Cannot reconnect: {e}"))?;
            *lock(&self.editor_cancel) = client.cancel_token();
        }
        Ok(client)
    }

    fn literal(&self, s: &str) -> String {
        pg_literal(s, self.std_strings.load(Ordering::Relaxed))
    }

    async fn table_info(&self, client: &Client, object: &ObjectRef) -> anyhow::Result<TableInfo> {
        let sql = format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                    pg_get_expr(d.adbin, d.adrelid), array_position(i.indkey::int2[], a.attnum), c.relkind \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
             LEFT JOIN pg_attrdef d ON d.adrelid = c.oid AND d.adnum = a.attnum \
             LEFT JOIN pg_index i ON i.indrelid = c.oid AND i.indisprimary \
             WHERE n.nspname = {} AND c.relname = {} \
             ORDER BY a.attnum",
            self.literal(&object.namespace),
            self.literal(&object.name)
        );
        let mut columns = Vec::new();
        let mut pk: Vec<(u32, String)> = Vec::new();
        let mut relkind = String::new();
        for row in query_rows(client, &sql).await? {
            let name = cell(&row, 0).unwrap_or_default().to_string();
            let pk_pos = cell(&row, 4).and_then(|s| s.parse::<u32>().ok());
            if let Some(p) = pk_pos {
                pk.push((p, name.clone()));
            }
            relkind = cell(&row, 5).unwrap_or_default().to_string();
            columns.push(ColumnInfo {
                name,
                type_name: cell(&row, 1).unwrap_or_default().to_string(),
                nullable: cell(&row, 2) == Some("t"),
                key: pk_pos.is_some(),
                default: cell(&row, 3).map(str::to_string),
            });
        }
        pk.sort();
        let types = columns
            .iter()
            .map(|c| (c.name.clone(), c.type_name.clone()))
            .collect();
        lock(&self.types).insert(object.clone(), types);
        Ok(TableInfo {
            columns,
            relkind,
            pk: pk.into_iter().map(|(_, name)| name).collect(),
        })
    }

    /// The statements for `changes`. `preview` shows them and `apply` runs them.
    fn statements(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<String>> {
        let std_strings = self.std_strings.load(Ordering::Relaxed);
        let types = lock(&self.types).get(object).cloned().unwrap_or_default();
        let table = qualified(Kind::Postgres, &object.namespace, &object.name);
        changes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                sql::change_sql(&table, c, &types, std_strings)
                    .map_err(|e| anyhow!("Change {}: {e}", i + 1))
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Driver for PgDriver {
    fn kind(&self) -> Kind {
        Kind::Postgres
    }

    async fn databases(&self) -> anyhow::Result<Vec<String>> {
        let client = self.meta().await?;
        first_column(&client, DATABASES_SQL).await
    }

    async fn namespaces(&self) -> anyhow::Result<Vec<String>> {
        let client = self.meta().await?;
        first_column(&client, NAMESPACES_SQL).await
    }

    fn default_namespace(&self) -> Option<String> {
        Some("public".into())
    }

    fn current_database(&self) -> Option<String> {
        self.opts.database()
    }

    async fn objects(&self, namespace: &str) -> anyhow::Result<Vec<ObjectInfo>> {
        let client = self.meta().await?;
        let sql = format!(
            "SELECT c.relname, c.relkind FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {} AND c.relkind IN ('r', 'p', 'f', 'v', 'm') \
             ORDER BY c.relname",
            self.literal(namespace)
        );
        let rows = query_rows(&client, &sql).await?;
        Ok(rows
            .iter()
            .map(|row| ObjectInfo {
                name: cell(row, 0).unwrap_or_default().to_string(),
                kind: match cell(row, 1) {
                    Some("v" | "m") => ObjectKind::View,
                    _ => ObjectKind::Table,
                },
            })
            .collect())
    }

    async fn columns(&self, object: &ObjectRef) -> anyhow::Result<Vec<ColumnInfo>> {
        let client = self.meta().await?;
        Ok(self.table_info(&client, object).await?.columns)
    }

    async fn browse(&self, req: &BrowseRequest) -> anyhow::Result<Page> {
        let client = self.meta().await?;
        let info = self.table_info(&client, &req.object).await?;
        let sql = sql::browse_sql(req, &info.pk)?;
        let mut out = Vec::new();
        run_simple(&client, &sql, usize::MAX, &mut out)
            .await
            .map_err(|e| filter_error(&e, &sql, &req.filter))?;
        drop(client);

        let mut result = out.into_iter().next().unwrap_or_default();
        let types: HashMap<&str, &str> = info
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.type_name.as_str()))
            .collect();
        result.types = result
            .columns
            .iter()
            .map(|c| types.get(c.as_str()).unwrap_or(&"").to_string())
            .collect();

        let reason = if self.read_only {
            Some("read-only connection")
        } else if matches!(info.relkind.as_str(), "v" | "m") {
            Some("view")
        } else if info.pk.is_empty() {
            Some("no primary key")
        } else {
            None
        };
        let key = if reason.is_none() {
            info.pk
        } else {
            Vec::new()
        };
        Ok(Page {
            result,
            key,
            read_only_reason: reason.map(str::to_string),
        })
    }

    async fn count(&self, req: &BrowseRequest) -> anyhow::Result<u64> {
        let sql = sql::count_sql(req)?;
        let client = self.meta().await?;
        let mut out = Vec::new();
        run_simple(&client, &sql, 1, &mut out)
            .await
            .map_err(|e| filter_error(&e, &sql, &req.filter))?;
        out.first()
            .and_then(|rs| rs.rows.first())
            .and_then(|row| cell(row, 0))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("The server sent no row count."))
    }

    async fn execute(
        &self,
        namespace: Option<&str>,
        text: &str,
        max_rows: usize,
    ) -> anyhow::Result<Vec<ResultSet>> {
        let client = self.editor().await?;
        if let Some(ns) = namespace.filter(|ns| !ns.is_empty()) {
            let path = if ns == "public" {
                "public".to_string()
            } else {
                format!("{}, public", quote_ident(Kind::Postgres, ns))
            };
            if let Err(e) = client
                .batch_execute(&format!("SET search_path TO {path}"))
                .await
            {
                // In a failed transaction every command fails until ROLLBACK.
                // Run the text anyway, so that the user can roll back.
                if e.code() != Some(&SqlState::IN_FAILED_SQL_TRANSACTION) {
                    bail!(error_text(&e));
                }
            }
        }

        // Send one statement at a time, like psql. Each statement commits on
        // its own (unless the user opened a transaction), and the error
        // tells which statement failed.
        let stmts: Vec<&str> = split_statements(text)
            .into_iter()
            .filter(|s| !sql::skip_comments(s).is_empty())
            .collect();
        let mut out = Vec::new();
        for stmt in &stmts {
            let label = |n: usize| {
                if stmts.len() > 1 {
                    format!("Statement {n}: ")
                } else {
                    String::new()
                }
            };
            if sql::is_copy_stdio(stmt) {
                bail!(
                    "{}COPY with STDIN or STDOUT does not work here. Use a server file, or \\copy in psql.",
                    label(out.len() + 1)
                );
            }
            let first = out.len();
            if let Err(e) = run_simple(&client, stmt, max_rows, &mut out).await {
                // `split_statements` gives slices of `text`.
                let offset = (stmt.as_ptr() as usize).saturating_sub(text.as_ptr() as usize);
                bail!(
                    "{}{}",
                    label(out.len() + 1),
                    error_text_with(&e, |p| {
                        let (line, col) = line_col(text, offset, p);
                        Some(format!("At line {line}, column {col}."))
                    })
                );
            }
            let word = sql::first_word(stmt);
            for (i, rs) in out[first..].iter_mut().enumerate() {
                if let Some(n) = rs.affected {
                    rs.message = Some(sql::status_text(if i == 0 { &word } else { "" }, n));
                }
            }
        }
        Ok(out)
    }

    async fn cancel(&self) {
        let token = lock(&self.editor_cancel).clone();
        self.opts.cancel(&token).await;
    }

    fn preview(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<String>> {
        self.statements(object, changes)
    }

    async fn apply(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<u64> {
        if self.read_only {
            bail!("This connection is read-only.");
        }
        let mut client = self.meta().await?;
        let stmts = self.statements(object, changes)?;
        let total = stmts.len();
        let tx = client
            .transaction()
            .await
            .map_err(|e| anyhow!(error_text(&e)))?;
        for (i, (stmt, change)) in stmts.iter().zip(changes).enumerate() {
            let failed = |why: String| {
                anyhow!(
                    "Change {} of {total} failed. Nothing was saved.\n{stmt}\n{why}",
                    i + 1
                )
            };
            let must_hit_one = matches!(change, Change::Update { .. } | Change::Delete { .. });
            match tx.execute(stmt.as_str(), &[]).await {
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(failed(error_text(&e)));
                }
                Ok(n) if must_hit_one && n != 1 => {
                    let _ = tx.rollback().await;
                    return Err(failed(format!(
                        "It changed {n} rows. It must change exactly 1 row."
                    )));
                }
                Ok(_) => {}
            }
        }
        tx.commit()
            .await
            .map_err(|e| anyhow!("Saving failed: {}", error_text(&e)))?;
        Ok(total as u64)
    }
}

// ---------------------------------------------------------------------------
// Simple query helpers
// ---------------------------------------------------------------------------

/// Run SQL with the simple query protocol and push one result per statement
/// to `out`. A result keeps at most `max_rows` rows.
async fn run_simple(
    client: &Client,
    sql: &str,
    max_rows: usize,
    out: &mut Vec<ResultSet>,
) -> Result<(), tokio_postgres::Error> {
    let mut stream = pin!(client.simple_query_raw(sql).await?);
    let mut current: Option<ResultSet> = None;
    let mut started = Instant::now();
    while let Some(msg) = stream.try_next().await? {
        match msg {
            SimpleQueryMessage::RowDescription(cols) => {
                let columns: Vec<String> = cols.iter().map(|c| c.name().to_string()).collect();
                current = Some(ResultSet {
                    types: vec![String::new(); columns.len()],
                    columns,
                    ..Default::default()
                });
            }
            SimpleQueryMessage::Row(row) => {
                if let Some(rs) = current.as_mut() {
                    if rs.rows.len() < max_rows {
                        rs.rows.push(row_values(&row)?);
                    } else {
                        rs.truncated = true;
                    }
                }
            }
            SimpleQueryMessage::CommandComplete(n) => {
                // A statement without a row description has no rows.
                let mut rs = current.take().unwrap_or_else(|| ResultSet {
                    affected: Some(n),
                    ..Default::default()
                });
                rs.elapsed = started.elapsed();
                started = Instant::now();
                out.push(rs);
            }
            _ => {}
        }
    }
    Ok(())
}

fn row_values(row: &SimpleQueryRow) -> Result<Vec<Value>, tokio_postgres::Error> {
    (0..row.len())
        .map(|i| {
            Ok(row
                .try_get(i)?
                .map_or(Value::Null, |s| Value::Text(s.to_string())))
        })
        .collect()
}

/// Error text for a browse or count query. The user does not see that query,
/// so a position in the filter becomes a position in the filter, and other
/// positions are dropped. The filter starts on line 2 (see `sql::where_clause`).
fn filter_error(e: &tokio_postgres::Error, sql: &str, filter: &str) -> anyhow::Error {
    let lines = filter.trim().lines().count();
    anyhow!(error_text_with(e, |p| {
        let (line, col) = line_col(sql, 0, p);
        let line = line.checked_sub(1).filter(|l| (1..=lines).contains(l))?;
        Some(if lines == 1 {
            format!("At column {col} of the filter.")
        } else {
            format!("At line {line}, column {col} of the filter.")
        })
    }))
}

/// Rows of a catalog query (text or NULL).
async fn query_rows(client: &Client, sql: &str) -> anyhow::Result<Vec<Vec<Value>>> {
    let mut out = Vec::new();
    run_simple(client, sql, usize::MAX, &mut out)
        .await
        .map_err(|e| anyhow!(error_text(&e)))?;
    Ok(out.into_iter().next().map(|rs| rs.rows).unwrap_or_default())
}

async fn first_column(client: &Client, sql: &str) -> anyhow::Result<Vec<String>> {
    let rows = query_rows(client, sql).await?;
    Ok(rows
        .iter()
        .filter_map(|row| cell(row, 0).map(str::to_string))
        .collect())
}

async fn read_std_strings(client: &Client) -> anyhow::Result<bool> {
    let rows = query_rows(client, "SHOW standard_conforming_strings").await?;
    Ok(rows.first().and_then(|row| cell(row, 0)) != Some("off"))
}

/// Text of cell `i`; `None` for NULL.
fn cell(row: &[Value], i: usize) -> Option<&str> {
    match row.get(i) {
        Some(Value::Text(s)) => Some(s),
        _ => None,
    }
}

fn lock<T>(m: &StdMutex<T>) -> StdMutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
