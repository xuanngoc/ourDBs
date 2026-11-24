//! OurDB driver for MySQL: mysql_async with the text protocol and native-tls.
//!
//! A small pool serves metadata, browse, count and apply. The query editor
//! has one dedicated connection, so session state persists between runs.

mod convert;
mod edit;

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use mysql_async::{
    Conn, DriverError, IoError, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, Row, SslOpts,
    TxOpts, prelude::Queryable,
};
use ourdb_core::{
    BrowseRequest, Change, ColumnInfo, ConnConfig, Driver, Kind, ObjectInfo, ObjectKind, ObjectRef,
    Page, ResultSet, TlsMode, Value,
    sql::{mysql_literal, split_statements},
};
use tokio::sync::Mutex;

use crate::edit::{q, table_name};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POOL_MAX: usize = 4;
const READ_ONLY_SQL: &str = "SET SESSION TRANSACTION READ ONLY";

/// Open a connection. The password is already in `cfg.password`.
pub async fn connect(cfg: &ConnConfig) -> anyhow::Result<Arc<dyn Driver>> {
    let base = base_opts(cfg)?;
    let (opts, mut conn) = open_first(cfg, base).await?;

    let sql_mode: Option<String> = conn
        .query_first("SELECT @@SESSION.sql_mode")
        .await
        .map_err(my_err)?;
    let backslash_escapes = !sql_mode
        .unwrap_or_default()
        .to_ascii_uppercase()
        .contains("NO_BACKSLASH_ESCAPES");

    let driver = MySql {
        pool: Pool::new(opts.clone()),
        default_ns: opts.db_name().filter(|s| !s.is_empty()).map(String::from),
        editor_id: AtomicU32::new(conn.id()),
        editor: Mutex::new(Some(conn)),
        running: AtomicBool::new(false),
        opts,
        backslash_escapes,
        read_only: cfg.read_only,
        binary_cols: StdMutex::new(HashMap::new()),
    };
    Ok(Arc::new(driver))
}

/// Connection options from the URL or the fields, without TLS.
fn base_opts(cfg: &ConnConfig) -> anyhow::Result<OptsBuilder> {
    let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let mut b = if cfg.has_url() {
        let opts = Opts::from_url(cfg.url.trim()).map_err(|e| anyhow!("Invalid MySQL URL: {e}"))?;
        let url_has_pass = opts.pass().is_some_and(|p| !p.is_empty());
        let mut b = OptsBuilder::from_opts(opts);
        if !url_has_pass {
            b = b.pass(non_empty(&cfg.password));
        }
        b
    } else {
        OptsBuilder::default()
            .user(non_empty(&cfg.user))
            .pass(non_empty(&cfg.password))
            .db_name(non_empty(cfg.database.trim()))
    };
    // Through a tunnel the host and port fields point at the local tunnel end.
    if !cfg.has_url() || cfg.via_tunnel {
        b = b
            .ip_or_hostname(cfg.host_or_default())
            .tcp_port(cfg.port_or_default());
    }
    // Keep up to POOL_MAX idle connections; after 5 idle minutes keep one.
    // (With an idle TTL of 0 the pool would keep only `min` connections.)
    let pool = PoolOpts::default()
        .with_constraints(PoolConstraints::new(1, POOL_MAX).context("bad pool size")?)
        .with_inactive_connection_ttl(Duration::from_secs(300))
        // We never change session state on pool connections, so skip the reset.
        .with_reset_connection(false);
    // `setup` runs on each new connection (and after a reset, unlike `init`).
    let setup = if cfg.read_only {
        vec![READ_ONLY_SQL]
    } else {
        Vec::new()
    };
    Ok(b
        // Else mysql_async can silently switch to a local socket, also through a tunnel.
        .prefer_socket(false)
        // An UPDATE that does not change the value still counts as 1 row.
        .client_found_rows(true)
        .setup(setup)
        .pool_opts(pool))
}

/// TLS options for the mode. Prefer and Require skip all checks.
fn ssl_opts(cfg: &ConnConfig) -> anyhow::Result<Option<SslOpts>> {
    Ok(match cfg.tls {
        TlsMode::Off => None,
        TlsMode::Prefer | TlsMode::Require => Some(
            SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        ),
        TlsMode::Verify => {
            // Through a tunnel the host name never matches the certificate.
            let mut ssl = SslOpts::default().with_danger_skip_domain_validation(cfg.via_tunnel);
            let ca = cfg.ca_file.trim();
            if !ca.is_empty() {
                let pem =
                    std::fs::read(ca).with_context(|| format!("Cannot read the CA file {ca}"))?;
                ssl = ssl.with_root_certs(vec![pem.into()]);
            }
            Some(ssl)
        }
    })
}

/// Open the first connection and choose the final options. TLS Prefer falls
/// back to plain TCP when TLS fails.
async fn open_first(cfg: &ConnConfig, base: OptsBuilder) -> anyhow::Result<(Opts, Conn)> {
    let opts: Opts = base.clone().ssl_opts(ssl_opts(cfg)?).into();
    let addr = format!("{}:{}", opts.ip_or_hostname(), opts.tcp_port());
    let fail =
        |e: mysql_async::Error| anyhow!("Cannot connect to MySQL at {addr}: {}", err_text(&e));
    match new_conn(&opts).await {
        Ok(conn) => Ok((opts, conn)),
        Err(e) if cfg.tls == TlsMode::Prefer && is_tls_error(&e) => {
            let plain: Opts = base.ssl_opts(None::<SslOpts>).into();
            let conn = new_conn(&plain).await.map_err(fail)?;
            Ok((plain, conn))
        }
        Err(e) => Err(fail(e)),
    }
}

/// A new connection, with the connect timeout.
async fn new_conn(opts: &Opts) -> Result<Conn, mysql_async::Error> {
    match tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts.clone())).await {
        Ok(result) => result,
        Err(_) => Err(timeout_error("no answer within 10 s")),
    }
}

fn timeout_error(what: &str) -> mysql_async::Error {
    let io = std::io::Error::new(std::io::ErrorKind::TimedOut, what.to_string());
    mysql_async::Error::Io(IoError::Io(io))
}

fn is_tls_error(e: &mysql_async::Error) -> bool {
    matches!(
        e,
        mysql_async::Error::Io(IoError::Tls(_))
            | mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer)
    )
}

/// Error text without Rust debug output. Server errors look like
/// "ERROR 1146 (42S02): Table 'x' doesn't exist".
fn err_text(e: &mysql_async::Error) -> String {
    use mysql_async::Error as E;
    match e {
        E::Server(s) => s.to_string(),
        E::Io(IoError::Io(io)) => format!("connection error: {io}"),
        E::Io(IoError::Tls(t)) => format!("TLS error: {t}"),
        E::Driver(d) => d.to_string(),
        E::Url(u) => format!("invalid URL: {u}"),
        E::Other(o) => o.to_string(),
    }
}

fn my_err(e: mysql_async::Error) -> anyhow::Error {
    anyhow!(err_text(&e))
}

/// Text of a metadata cell.
fn text(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Value::Null | Value::Missing => String::new(),
        other => other.edit_text(),
    }
}

/// Sets a flag while it lives and clears it on drop (also when a future is dropped).
struct Busy<'a>(&'a AtomicBool);

impl<'a> Busy<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::SeqCst);
        Busy(flag)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Run SQL text (one or more statements) and read every result set. At most
/// `max_rows` rows are kept per result. On error, gives the 0-based number of
/// the result set that failed.
async fn run_sql(
    conn: &mut Conn,
    sql: &str,
    max_rows: usize,
) -> Result<Vec<ResultSet>, (usize, mysql_async::Error)> {
    let mut start = Instant::now();
    let mut result = conn.query_iter(sql).await.map_err(|e| (0, e))?;
    let mut sets = Vec::new();
    loop {
        // `columns()` is None at the end and when the next statement failed.
        // Read the OK packet values before the rows: the next set replaces them.
        let cols = result.columns();
        let affected = result.affected_rows();
        let info = result.info().into_owned();
        let binary: Vec<bool> = cols
            .iter()
            .flat_map(|c| c.iter())
            .map(convert::is_binary)
            .collect();
        // `reduce` reads one result set, or returns the error of a failed statement.
        let (rows, total) = result
            .reduce::<Row, _, _>((Vec::new(), 0usize), |(mut rows, n), row| {
                if n < max_rows {
                    rows.push(convert::row_values(row, &binary));
                }
                (rows, n + 1)
            })
            .await
            .map_err(|e| (sets.len(), e))?;
        let Some(cols) = cols else { break };

        let mut rs = ResultSet {
            elapsed: start.elapsed(),
            ..Default::default()
        };
        start = Instant::now();
        if cols.is_empty() {
            let mut msg = format!(
                "{affected} row{} affected",
                if affected == 1 { "" } else { "s" }
            );
            if !info.is_empty() {
                msg.push_str(&format!(" ({info})"));
            }
            rs.affected = Some(affected);
            rs.message = Some(msg);
        } else {
            rs.columns = cols.iter().map(|c| c.name_str().into_owned()).collect();
            rs.types = cols.iter().map(convert::type_name).collect();
            rs.truncated = total > rows.len();
            rs.rows = rows;
        }
        sets.push(rs);
    }
    Ok(sets)
}

/// The error of a failed editor run. It names the statement when the text has
/// more than one, unless a CALL can return extra result sets.
fn statement_error(text: &str, index: usize, e: &mysql_async::Error) -> anyhow::Error {
    let stmts = split_statements(text);
    let has_call = stmts
        .iter()
        .any(|s| s.get(..5).is_some_and(|w| w.eq_ignore_ascii_case("call ")));
    if stmts.len() > 1 && !has_call {
        anyhow!("Statement {}: {}", index + 1, err_text(e))
    } else {
        anyhow!(err_text(e))
    }
}

struct TableMeta {
    is_view: bool,
    /// Primary key columns in index order.
    key: Vec<String>,
}

struct MySql {
    opts: Opts,
    pool: Pool,
    /// The query editor connection. `None` until the next run reconnects.
    editor: Mutex<Option<Conn>>,
    /// Server id of the editor connection, for KILL QUERY.
    editor_id: AtomicU32,
    /// True while the editor runs a query.
    running: AtomicBool,
    backslash_escapes: bool,
    read_only: bool,
    default_ns: Option<String>,
    /// Binary columns per table, from the last browse. Edits need them for `0x` input.
    binary_cols: StdMutex<HashMap<ObjectRef, HashSet<String>>>,
}

impl MySql {
    /// A pool connection. A slot or a new connection must come within 10 s.
    async fn pool_conn(&self) -> anyhow::Result<Conn> {
        match tokio::time::timeout(CONNECT_TIMEOUT, self.pool.get_conn()).await {
            Ok(result) => result.map_err(my_err),
            Err(_) => bail!(
                "No MySQL connection within 10 s: the server does not answer, or all {POOL_MAX} connections are busy."
            ),
        }
    }

    /// Run SQL on a pool connection.
    async fn pool_sql(&self, sql: &str) -> anyhow::Result<Vec<ResultSet>> {
        let mut conn = self.pool_conn().await?;
        run_sql(&mut conn, sql, usize::MAX)
            .await
            .map_err(|(_, e)| my_err(e))
    }

    /// A `TABLE_SCHEMA = ...` condition. An empty namespace means the current database.
    fn schema_cond(&self, namespace: &str) -> String {
        if namespace.is_empty() {
            "TABLE_SCHEMA = DATABASE()".into()
        } else {
            format!("TABLE_SCHEMA = {}", self.lit(namespace))
        }
    }

    fn lit(&self, s: &str) -> String {
        mysql_literal(s, self.backslash_escapes)
    }

    async fn table_meta(&self, obj: &ObjectRef) -> anyhow::Result<TableMeta> {
        let (schema, name) = (self.schema_cond(&obj.namespace), self.lit(&obj.name));
        let sql = format!(
            "SELECT TABLE_TYPE FROM information_schema.TABLES WHERE {schema} AND TABLE_NAME = {name}; \
             SELECT COLUMN_NAME FROM information_schema.STATISTICS \
             WHERE {schema} AND TABLE_NAME = {name} AND INDEX_NAME = 'PRIMARY' ORDER BY SEQ_IN_INDEX"
        );
        let sets = self.pool_sql(&sql).await?;
        let first_cell = |i: usize| {
            sets.get(i)
                .into_iter()
                .flat_map(|s| s.rows.iter())
                .map(|r| text(&r[0]))
        };
        let table_type: Vec<String> = first_cell(0).collect();
        let is_view = table_type.iter().any(|t| t.contains("VIEW"));
        Ok(TableMeta {
            is_view,
            key: first_cell(1).collect(),
        })
    }

    fn statements(&self, obj: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<edit::Stmt>> {
        let binary = {
            let map = self.binary_cols.lock().unwrap_or_else(|e| e.into_inner());
            map.get(obj).cloned().unwrap_or_default()
        };
        let lit = edit::Literals {
            backslash_escapes: self.backslash_escapes,
            binary: &binary,
        };
        edit::build(obj, changes, &lit)
    }

    /// The editor connection: the old one if it still answers, else a new one.
    async fn editor_conn(&self, slot: &mut Option<Conn>) -> anyhow::Result<Conn> {
        if let Some(mut conn) = slot.take()
            && conn.ping().await.is_ok()
        {
            return Ok(conn);
        }
        let conn = new_conn(&self.opts)
            .await
            .map_err(|e| anyhow!("Cannot reconnect: {}", err_text(&e)))?;
        self.editor_id.store(conn.id(), Ordering::SeqCst);
        Ok(conn)
    }
}

fn where_filter(filter: &str) -> String {
    let filter = filter.trim();
    // The new line ends a trailing `--` or `#` comment before the parenthesis.
    if filter.is_empty() {
        String::new()
    } else {
        format!(" WHERE ({filter}\n)")
    }
}

fn browse_sql(req: &BrowseRequest, key: &[String]) -> String {
    let mut sql = format!(
        "SELECT * FROM {}{}",
        table_name(&req.object),
        where_filter(&req.filter)
    );
    let order: Vec<String> = match &req.sort {
        Some((col, asc)) => {
            let dir = if *asc { "ASC" } else { "DESC" };
            // The key columns break ties, so pages stay stable.
            let ties = key.iter().filter(|k| *k != col).map(|k| q(k));
            std::iter::once(format!("{} {dir}", q(col)))
                .chain(ties)
                .collect()
        }
        None => key.iter().map(|k| q(k)).collect(),
    };
    if !order.is_empty() {
        sql.push_str(&format!(" ORDER BY {}", order.join(", ")));
    }
    sql.push_str(&format!(" LIMIT {} OFFSET {}", req.limit, req.offset));
    sql
}

#[async_trait::async_trait]
impl Driver for MySql {
    fn kind(&self) -> Kind {
        Kind::MySql
    }

    async fn namespaces(&self) -> anyhow::Result<Vec<String>> {
        let sets = self.pool_sql("SHOW DATABASES").await?;
        Ok(sets
            .iter()
            .flat_map(|s| s.rows.iter())
            .map(|r| text(&r[0]))
            .collect())
    }

    fn default_namespace(&self) -> Option<String> {
        self.default_ns.clone()
    }

    async fn objects(&self, namespace: &str) -> anyhow::Result<Vec<ObjectInfo>> {
        let sql = format!(
            "SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE {} ORDER BY TABLE_NAME",
            self.schema_cond(namespace)
        );
        let sets = self.pool_sql(&sql).await?;
        let objects = sets.iter().flat_map(|s| s.rows.iter()).map(|r| ObjectInfo {
            name: text(&r[0]),
            kind: if text(&r[1]).contains("VIEW") {
                ObjectKind::View
            } else {
                ObjectKind::Table
            },
        });
        Ok(objects.collect())
    }

    async fn columns(&self, object: &ObjectRef) -> anyhow::Result<Vec<ColumnInfo>> {
        let sql = format!(
            "SELECT c.COLUMN_NAME, c.COLUMN_TYPE, c.IS_NULLABLE, c.COLUMN_DEFAULT, s.SEQ_IN_INDEX \
             FROM information_schema.COLUMNS c \
             LEFT JOIN information_schema.STATISTICS s ON s.TABLE_SCHEMA = c.TABLE_SCHEMA \
               AND s.TABLE_NAME = c.TABLE_NAME AND s.COLUMN_NAME = c.COLUMN_NAME AND s.INDEX_NAME = 'PRIMARY' \
             WHERE c.{} AND c.TABLE_NAME = {} ORDER BY c.ORDINAL_POSITION",
            self.schema_cond(&object.namespace),
            self.lit(&object.name)
        );
        let sets = self.pool_sql(&sql).await?;
        let columns = sets.iter().flat_map(|s| s.rows.iter()).map(|r| ColumnInfo {
            name: text(&r[0]),
            type_name: text(&r[1]),
            nullable: text(&r[2]).eq_ignore_ascii_case("YES"),
            default: (!r[3].is_null()).then(|| text(&r[3])),
            key: !r[4].is_null(),
        });
        Ok(columns.collect())
    }

    async fn browse(&self, req: &BrowseRequest) -> anyhow::Result<Page> {
        let meta = self.table_meta(&req.object).await?;
        let sets = self.pool_sql(&browse_sql(req, &meta.key)).await?;
        let result = sets
            .into_iter()
            .find(|s| s.affected.is_none())
            .unwrap_or_default();

        let binary = result
            .columns
            .iter()
            .zip(&result.types)
            .filter(|(_, t)| convert::is_binary_type_name(t));
        let binary: HashSet<String> = binary.map(|(c, _)| c.clone()).collect();
        self.binary_cols
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(req.object.clone(), binary);

        let reason = if self.read_only {
            Some("read-only connection")
        } else if meta.is_view {
            Some("view")
        } else if meta.key.is_empty() {
            Some("no primary key")
        } else {
            None
        };
        let key = if reason.is_none() {
            meta.key
        } else {
            Vec::new()
        };
        Ok(Page {
            result,
            key,
            read_only_reason: reason.map(String::from),
        })
    }

    async fn count(&self, req: &BrowseRequest) -> anyhow::Result<u64> {
        let sql = format!(
            "SELECT COUNT(*) FROM {}{}",
            table_name(&req.object),
            where_filter(&req.filter)
        );
        let sets = self.pool_sql(&sql).await?;
        let cell = sets
            .first()
            .and_then(|s| s.rows.first())
            .map(|r| text(&r[0]))
            .unwrap_or_default();
        cell.parse()
            .with_context(|| format!("Unexpected count value '{cell}'"))
    }

    async fn execute(
        &self,
        namespace: Option<&str>,
        text: &str,
        max_rows: usize,
    ) -> anyhow::Result<Vec<ResultSet>> {
        if split_statements(text).is_empty() {
            return Ok(Vec::new());
        }
        let mut slot = self.editor.lock().await;
        // The connection is out of the slot while it works. If this future is
        // dropped mid-query, the connection goes too, and the next run reconnects.
        let mut conn = self.editor_conn(&mut slot).await?;
        if let Some(ns) = namespace.filter(|ns| !ns.is_empty())
            && let Err(e) = conn.query_drop(format!("USE {}", q(ns))).await
        {
            if !e.is_fatal() {
                *slot = Some(conn);
            }
            return Err(my_err(e));
        }
        let result = {
            let _busy = Busy::new(&self.running);
            run_sql(&mut conn, text, max_rows).await
        };
        match result {
            Ok(sets) => {
                *slot = Some(conn);
                Ok(sets)
            }
            Err((index, e)) => {
                if !e.is_fatal() {
                    *slot = Some(conn);
                }
                Err(statement_error(text, index, &e))
            }
        }
    }

    async fn cancel(&self) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        let id = self.editor_id.load(Ordering::SeqCst);
        if let Ok(mut conn) = self.pool_conn().await {
            let _ = conn.query_drop(format!("KILL QUERY {id}")).await;
        }
    }

    fn preview(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<String>> {
        Ok(self
            .statements(object, changes)?
            .into_iter()
            .map(|s| s.sql)
            .collect())
    }

    async fn apply(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<u64> {
        if self.read_only {
            bail!("This connection is read-only.");
        }
        let stmts = self.statements(object, changes)?;
        if stmts.is_empty() {
            return Ok(0);
        }
        let mut conn = self.pool_conn().await?;
        let mut tx = conn
            .start_transaction(TxOpts::default())
            .await
            .map_err(my_err)?;
        for (i, stmt) in stmts.iter().enumerate() {
            let problem = match tx.query_drop(stmt.sql.as_str()).await {
                Err(e) => Some(err_text(&e)),
                Ok(()) if stmt.one_row && tx.affected_rows() != 1 => Some(format!(
                    "it affected {} rows, but it must affect exactly 1",
                    tx.affected_rows()
                )),
                Ok(()) => None,
            };
            if let Some(problem) = problem {
                let _ = tx.rollback().await;
                bail!(
                    "Change {} failed: {problem}. Nothing was saved.\n{}",
                    i + 1,
                    stmt.sql
                );
            }
        }
        tx.commit().await.map_err(my_err)?;
        Ok(stmts.len() as u64)
    }

    async fn close(&self) {
        self.cancel().await;
        if let Some(conn) = self.editor.lock().await.take() {
            let _ = conn.disconnect().await;
        }
        let _ = self.pool.clone().disconnect().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(filter: &str, sort: Option<(&str, bool)>) -> BrowseRequest {
        BrowseRequest {
            object: ObjectRef::new("db", "t"),
            filter: filter.into(),
            sort: sort.map(|(c, a)| (c.to_string(), a)),
            limit: 50,
            offset: 100,
        }
    }

    #[test]
    fn browse_statements() {
        let key = vec!["b".to_string(), "a".to_string()];
        assert_eq!(
            browse_sql(&req("", None), &key),
            "SELECT * FROM `db`.`t` ORDER BY `b`, `a` LIMIT 50 OFFSET 100"
        );
        assert_eq!(
            browse_sql(&req(" ", None), &[]),
            "SELECT * FROM `db`.`t` LIMIT 50 OFFSET 100"
        );
        assert_eq!(
            browse_sql(&req("x > 1 -- note", Some(("a", false))), &key),
            "SELECT * FROM `db`.`t` WHERE (x > 1 -- note\n) ORDER BY `a` DESC, `b` LIMIT 50 OFFSET 100"
        );
    }

    #[test]
    fn statement_numbers() {
        let e = mysql_async::Error::Server(mysql_async::ServerError {
            code: 1146,
            message: "Table 'x' doesn't exist".into(),
            state: "42S02".into(),
        });
        assert_eq!(
            statement_error("select 1; select * from x", 1, &e).to_string(),
            "Statement 2: ERROR 1146 (42S02): Table 'x' doesn't exist"
        );
        assert_eq!(
            statement_error("select * from x", 0, &e).to_string(),
            "ERROR 1146 (42S02): Table 'x' doesn't exist"
        );
        assert!(
            !statement_error("call p(); select 1", 1, &e)
                .to_string()
                .starts_with("Statement")
        );
    }

    #[test]
    fn tls_errors() {
        assert!(is_tls_error(&mysql_async::Error::Driver(
            DriverError::NoClientSslFlagFromServer
        )));
        assert!(!is_tls_error(&timeout_error("x")));
    }
}
