//! OurDB UI demo with in-memory mock databases (no real database needed).
//!
//! Run: `cargo run -p ourdb-ui --example demo`
//! The demo writes its connections to a temp folder through OURDB_CONFIG_DIR,
//! so it never touches ~/Library/Application Support/OurDB or the Keychain.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail};
use ourdb_core::{
    BrowseRequest, Change, ColumnInfo, ConnConfig, Connector, Driver, Edit, Env, Kind, ObjectInfo,
    ObjectKind, ObjectRef, Page, ResultSet, Session, TlsMode, Value, sql,
};
use serde_json::{Map, Value as J, json};

fn main() -> anyhow::Result<()> {
    if std::env::var_os("OURDB_CONFIG_DIR").is_none() {
        let dir = std::env::temp_dir().join("ourdb-demo");
        std::fs::create_dir_all(&dir)?;
        // Safety: no other thread runs yet.
        unsafe { std::env::set_var("OURDB_CONFIG_DIR", &dir) };
    }
    let dir = std::path::PathBuf::from(std::env::var_os("OURDB_CONFIG_DIR").unwrap_or_default());
    std::fs::write(
        dir.join("connections.json"),
        serde_json::to_string_pretty(&seed_connections())?,
    )?;

    let connector: Connector = Arc::new(|cfg: ConnConfig| {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let driver: Arc<dyn Driver> = match cfg.kind {
                Kind::Postgres | Kind::MySql | Kind::Sqlite => {
                    Arc::new(SqlMock::new(cfg.kind, &cfg.database))
                }
                Kind::MongoDb => Arc::new(DocMock::new()),
                Kind::DocumentDb => {
                    bail!("Server selection timeout: no available servers (demo has no DocumentDB)")
                }
            };
            Ok(Session {
                driver,
                tunnel: None,
            })
        })
    });
    ourdb_ui::run(connector)
}

fn seed_connections() -> Vec<ConnConfig> {
    let c = |id: &str, name: &str, kind: Kind, env: Env| ConnConfig {
        id: id.into(),
        name: name.into(),
        kind,
        env,
        tls: kind.default_tls(),
        ..Default::default()
    };
    vec![
        ConnConfig {
            host: "localhost".into(),
            user: "ourdb".into(),
            database: "shop".into(),
            ..c("pg", "Local Postgres", Kind::Postgres, Env::Dev)
        },
        ConnConfig {
            host: "db.internal".into(),
            user: "app".into(),
            database: "billing".into(),
            ..c("my", "Billing MySQL", Kind::MySql, Env::Prod)
        },
        ConnConfig {
            host: "localhost".into(),
            database: "app".into(),
            ..c("mg", "Mongo Events", Kind::MongoDb, Env::Staging)
        },
        ConnConfig {
            host: "docdb.cluster-abc.ap-northeast-1.docdb.amazonaws.com".into(),
            user: "admin".into(),
            ssh: "bastion".into(),
            tls: TlsMode::Verify,
            ca_file: "~/certs/global-bundle.pem".into(),
            ..c("dd", "DocumentDB (AWS)", Kind::DocumentDb, Env::Prod)
        },
        ConnConfig {
            path: "/tmp/analytics.sqlite".into(),
            ..c("sl", "Analytics SQLite", Kind::Sqlite, Env::None)
        },
    ]
}

// ---------------------------------------------------------------------------
// SQL mock: public.users (1,000 rows), public.active_users (view), audit.log
// ---------------------------------------------------------------------------

struct Table {
    columns: Vec<ColumnInfo>,
    rows: Vec<Vec<Value>>,
    view: bool,
}

struct SqlMock {
    kind: Kind,
    db: String,
    tables: Mutex<Vec<(ObjectRef, Table)>>,
}

fn col(name: &str, ty: &str, key: bool, nullable: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        type_name: ty.into(),
        nullable,
        key,
        default: key.then(|| "nextval('users_id_seq')".into()),
    }
}

/// Small deterministic random numbers.
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

fn users() -> Table {
    let first = [
        "Ana", "Bao", "Chen", "Dara", "Emil", "Fumi", "Gita", "Hugo", "Ines", "Jun", "Kai", "Lena",
        "Minh", "Nora",
    ];
    let last = [
        "Nguyen", "Sato", "Smith", "Garcia", "Kim", "Müller", "Rossi", "Tran", "Silva", "Ito",
    ];
    let long = "This customer asked for a refund twice. The first request came by email, the second one by phone.\nWe agreed to a partial refund. See ticket #4821 for the full history and the notes from the support team.";
    let mut rng = Rng(42);
    let rows = (1..=1000u64)
        .map(|id| {
            let f = first[rng.next(first.len() as u64) as usize];
            let l = last[rng.next(last.len() as u64) as usize];
            let email = if id % 13 == 0 {
                String::new()
            } else {
                format!(
                    "{}.{}{}@example.com",
                    f.to_lowercase(),
                    l.to_lowercase(),
                    id
                )
            };
            let notes = match id % 10 {
                3 | 6 => Value::Null,
                7 => Value::Text(String::new()),
                0 => Value::Text(long.into()),
                _ => Value::Text(format!(
                    "Plan {}",
                    ["free", "pro", "team"][(id % 3) as usize]
                )),
            };
            vec![
                Value::Text(id.to_string()),
                Value::Text(format!("{f} {l}")),
                Value::Text(email),
                Value::Text((18 + rng.next(60)).to_string()),
                Value::Text(format!(
                    "2025-{:02}-{:02} {:02}:{:02}:00+00",
                    1 + rng.next(12),
                    1 + rng.next(28),
                    rng.next(24),
                    rng.next(60)
                )),
                notes,
            ]
        })
        .collect();
    Table {
        columns: vec![
            col("id", "integer", true, false),
            col("name", "text", false, false),
            col("email", "varchar(255)", false, true),
            col("age", "integer", false, true),
            col("created_at", "timestamp with time zone", false, false),
            col("notes", "text", false, true),
        ],
        rows,
        view: false,
    }
}

impl SqlMock {
    fn new(kind: Kind, db: &str) -> Self {
        let u = users();
        let active = Table {
            columns: u.columns[..4]
                .iter()
                .cloned()
                .map(|mut c| {
                    c.key = false;
                    c
                })
                .collect(),
            rows: u
                .rows
                .iter()
                .filter(|r| r[3].edit_text().parse::<u32>().unwrap_or(0) < 40)
                .map(|r| r[..4].to_vec())
                .collect(),
            view: true,
        };
        let log = Table {
            columns: vec![
                col("id", "bigint", true, false),
                col("action", "text", false, false),
                col("payload", "jsonb", false, true),
            ],
            rows: (1..=25)
                .map(|i| {
                    vec![
                        Value::Text(i.to_string()),
                        Value::Text(["login", "logout", "update"][i % 3].into()),
                        Value::Text(format!("{{\"user\": {i}}}")),
                    ]
                })
                .collect(),
            view: false,
        };
        Self {
            kind,
            db: db.to_string(),
            tables: Mutex::new(vec![
                (ObjectRef::new("public", "users"), u),
                (ObjectRef::new("public", "active_users"), active),
                (ObjectRef::new("audit", "log"), log),
            ]),
        }
    }
}

/// Supports `<column> <op> <number or 'text'>` with op in =, <, >, <=, >=, and `<column> IS NULL`.
type RowFilter = Box<dyn Fn(&[Value]) -> bool>;

fn sql_filter(filter: &str, columns: &[ColumnInfo]) -> anyhow::Result<RowFilter> {
    let f = filter.trim();
    if f.is_empty() {
        return Ok(Box::new(|_| true));
    }
    let col_idx = |name: &str| {
        let name = name.trim().trim_matches('"').trim_matches('`');
        columns
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| anyhow!("column \"{name}\" does not exist"))
    };
    if let Some(name) = f
        .strip_suffix(" IS NULL")
        .or_else(|| f.strip_suffix(" is null"))
    {
        let i = col_idx(name)?;
        return Ok(Box::new(move |r| r[i].is_null()));
    }
    for op in [">=", "<=", "=", ">", "<"] {
        if let Some((l, r)) = f.split_once(op) {
            let i = col_idx(l)?;
            let rhs = r.trim().trim_matches('\'').to_string();
            let op = op.to_string();
            return Ok(Box::new(move |row| {
                let v = row[i].edit_text();
                let ord = match (v.parse::<f64>(), rhs.parse::<f64>()) {
                    (Ok(a), Ok(b)) => a.partial_cmp(&b),
                    _ => Some(v.as_str().cmp(rhs.as_str())),
                };
                let Some(ord) = ord else { return false };
                if row[i].is_null() {
                    return false;
                }
                match op.as_str() {
                    ">=" => ord.is_ge(),
                    "<=" => ord.is_le(),
                    ">" => ord.is_gt(),
                    "<" => ord.is_lt(),
                    _ => ord.is_eq(),
                }
            }));
        }
    }
    bail!("syntax error at or near \"{f}\"")
}

fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    let (x, y) = (a.edit_text(), b.edit_text());
    match (a.is_null(), b.is_null()) {
        (true, true) => return std::cmp::Ordering::Equal,
        (true, false) => return std::cmp::Ordering::Greater,
        (false, true) => return std::cmp::Ordering::Less,
        _ => {}
    }
    match (x.parse::<f64>(), y.parse::<f64>()) {
        (Ok(p), Ok(q)) => p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal),
        _ => x.cmp(&y),
    }
}

fn result_set(columns: &[ColumnInfo], rows: Vec<Vec<Value>>, elapsed: Duration) -> ResultSet {
    ResultSet {
        columns: columns.iter().map(|c| c.name.clone()).collect(),
        types: columns.iter().map(|c| c.type_name.clone()).collect(),
        rows,
        elapsed,
        ..Default::default()
    }
}

#[async_trait::async_trait]
impl Driver for SqlMock {
    fn kind(&self) -> Kind {
        self.kind
    }
    async fn databases(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["shop".into(), "postgres".into(), "analytics".into()])
    }
    async fn namespaces(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["audit".into(), "public".into()])
    }
    fn default_namespace(&self) -> Option<String> {
        Some("public".into())
    }
    async fn objects(&self, namespace: &str) -> anyhow::Result<Vec<ObjectInfo>> {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let tables = self.tables.lock().unwrap();
        let mut out: Vec<ObjectInfo> = tables
            .iter()
            .filter(|(o, _)| o.namespace == namespace)
            .map(|(o, t)| ObjectInfo {
                name: o.name.clone(),
                kind: if t.view {
                    ObjectKind::View
                } else {
                    ObjectKind::Table
                },
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
    async fn columns(&self, object: &ObjectRef) -> anyhow::Result<Vec<ColumnInfo>> {
        let tables = self.tables.lock().unwrap();
        let (_, t) = tables
            .iter()
            .find(|(o, _)| o == object)
            .ok_or_else(|| anyhow!("relation \"{}\" does not exist", object.name))?;
        Ok(t.columns.clone())
    }
    async fn browse(&self, req: &BrowseRequest) -> anyhow::Result<Page> {
        tokio::time::sleep(Duration::from_millis(80)).await;
        let t0 = Instant::now();
        let tables = self.tables.lock().unwrap();
        let (_, t) = tables
            .iter()
            .find(|(o, _)| *o == req.object)
            .ok_or_else(|| anyhow!("relation \"{}\" does not exist", req.object.name))?;
        let keep = sql_filter(&req.filter, &t.columns)?;
        let mut rows: Vec<Vec<Value>> = t.rows.iter().filter(|r| keep(r)).cloned().collect();
        if let Some((name, asc)) = &req.sort {
            let i = t
                .columns
                .iter()
                .position(|c| &c.name == name)
                .ok_or_else(|| anyhow!("column \"{name}\" does not exist"))?;
            rows.sort_by(|a, b| {
                if *asc {
                    cmp_values(&a[i], &b[i])
                } else {
                    cmp_values(&b[i], &a[i])
                }
            });
        }
        let rows = rows
            .into_iter()
            .skip(req.offset as usize)
            .take(req.limit as usize)
            .collect();
        let key: Vec<String> = if t.view {
            vec![]
        } else {
            t.columns
                .iter()
                .filter(|c| c.key)
                .map(|c| c.name.clone())
                .collect()
        };
        Ok(Page {
            result: result_set(&t.columns, rows, t0.elapsed()),
            read_only_reason: key.is_empty().then(|| "view".to_string()),
            key,
        })
    }
    async fn count(&self, req: &BrowseRequest) -> anyhow::Result<u64> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let tables = self.tables.lock().unwrap();
        let (_, t) = tables
            .iter()
            .find(|(o, _)| *o == req.object)
            .ok_or_else(|| anyhow!("relation does not exist"))?;
        let keep = sql_filter(&req.filter, &t.columns)?;
        Ok(t.rows.iter().filter(|r| keep(r)).count() as u64)
    }
    async fn execute(
        &self,
        _ns: Option<&str>,
        text: &str,
        max_rows: usize,
    ) -> anyhow::Result<Vec<ResultSet>> {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let tables = self.tables.lock().unwrap();
        let mut out = Vec::new();
        for (n, stmt) in sql::split_statements(text).into_iter().enumerate() {
            let lower = stmt.to_lowercase();
            if lower.starts_with("select") {
                let name = lower
                    .split(" from ")
                    .nth(1)
                    .and_then(|r| r.split_whitespace().next())
                    .unwrap_or("");
                let name = name.rsplit('.').next().unwrap_or("").trim_matches('"');
                let Some((_, t)) = tables.iter().find(|(o, _)| o.name == name) else {
                    if name.is_empty() {
                        let mut rs = result_set(
                            &[col("?column?", "integer", false, true)],
                            vec![vec![Value::Text("1".into())]],
                            Duration::from_millis(1),
                        );
                        rs.columns = vec!["?column?".into()];
                        out.push(rs);
                        continue;
                    }
                    bail!("statement {}: relation \"{name}\" does not exist", n + 1);
                };
                let mut rows: Vec<Vec<Value>> = t.rows.clone();
                let truncated = rows.len() > max_rows;
                rows.truncate(max_rows);
                let mut rs = result_set(&t.columns, rows, Duration::from_millis(7));
                rs.truncated = truncated;
                out.push(rs);
            } else {
                let word = lower.split_whitespace().next().unwrap_or("").to_uppercase();
                out.push(ResultSet {
                    affected: Some(3),
                    message: Some(format!("{word} 3")),
                    elapsed: Duration::from_millis(4),
                    ..Default::default()
                });
            }
        }
        Ok(out)
    }
    fn preview(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<String>> {
        let table = sql::qualified(self.kind, &object.namespace, &object.name);
        let q = |s: &str| sql::quote_ident(self.kind, s);
        let lit = |e: &Edit| match e {
            Edit::Null => "NULL".to_string(),
            Edit::Text(t) => sql::pg_literal(t, true),
        };
        let where_ = |key: &[(String, Value)]| {
            key.iter()
                .map(|(c, v)| format!("{} = {}", q(c), sql::pg_literal(&v.edit_text(), true)))
                .collect::<Vec<_>>()
                .join(" AND ")
        };
        changes
            .iter()
            .map(|c| match c {
                Change::Update { key, set } => Ok(format!(
                    "UPDATE {table} SET {} WHERE {}",
                    set.iter()
                        .map(|(c, _, e)| format!("{} = {}", q(c), lit(e)))
                        .collect::<Vec<_>>()
                        .join(", "),
                    where_(key)
                )),
                Change::Insert { values } if values.is_empty() => {
                    Ok(format!("INSERT INTO {table} DEFAULT VALUES"))
                }
                Change::Insert { values } => Ok(format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    values
                        .iter()
                        .map(|(c, _)| q(c))
                        .collect::<Vec<_>>()
                        .join(", "),
                    values
                        .iter()
                        .map(|(_, e)| lit(e))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                Change::Delete { key } => Ok(format!("DELETE FROM {table} WHERE {}", where_(key))),
                _ => bail!("documents are not supported here"),
            })
            .collect()
    }
    async fn apply(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<u64> {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut tables = self.tables.lock().unwrap();
        let (_, t) = tables
            .iter_mut()
            .find(|(o, _)| o == object)
            .ok_or_else(|| anyhow!("relation does not exist"))?;
        let mut rows = t.rows.clone(); // All or nothing.
        let find = |rows: &Vec<Vec<Value>>, key: &[(String, Value)]| {
            rows.iter().position(|r| {
                key.iter().all(|(c, v)| {
                    t.columns
                        .iter()
                        .position(|ci| &ci.name == c)
                        .is_some_and(|i| r[i] == *v)
                })
            })
        };
        for (n, c) in changes.iter().enumerate() {
            match c {
                Change::Update { key, set } => {
                    let i = find(&rows, key).ok_or_else(|| {
                        anyhow!("change {}: the row was not found (0 rows updated)", n + 1)
                    })?;
                    for (cname, _, e) in set {
                        let ci = t
                            .columns
                            .iter()
                            .position(|x| &x.name == cname)
                            .ok_or_else(|| anyhow!("no column {cname}"))?;
                        if t.columns[ci].type_name == "integer"
                            && matches!(e, Edit::Text(s) if s.parse::<i64>().is_err())
                        {
                            bail!("change {}: invalid input syntax for type integer", n + 1);
                        }
                        rows[i][ci] = match e {
                            Edit::Null => Value::Null,
                            Edit::Text(s) => Value::Text(s.clone()),
                        };
                    }
                }
                Change::Insert { values } => {
                    let next = rows
                        .iter()
                        .filter_map(|r| r[0].edit_text().parse::<u64>().ok())
                        .max()
                        .unwrap_or(0)
                        + 1;
                    let mut row: Vec<Value> = t.columns.iter().map(|_| Value::Null).collect();
                    row[0] = Value::Text(next.to_string());
                    for (cname, e) in values {
                        if let Some(ci) = t.columns.iter().position(|x| &x.name == cname) {
                            row[ci] = match e {
                                Edit::Null => Value::Null,
                                Edit::Text(s) => Value::Text(s.clone()),
                            };
                        }
                    }
                    rows.push(row);
                }
                Change::Delete { key } => {
                    let i = find(&rows, key)
                        .ok_or_else(|| anyhow!("change {}: the row was not found", n + 1))?;
                    rows.remove(i);
                }
                _ => bail!("documents are not supported here"),
            }
        }
        t.rows = rows;
        let _ = &self.db;
        Ok(changes.len() as u64)
    }
}

// ---------------------------------------------------------------------------
// Document mock: app.events (Extended JSON values, missing fields), app.users
// ---------------------------------------------------------------------------

type Doc = Map<String, J>;

struct DocMock {
    colls: Mutex<Vec<(ObjectRef, Vec<Doc>)>>,
}

fn oid(i: u64) -> J {
    json!({"$oid": format!("65a1f0c2a1b2c3d4e5f6{i:04x}")})
}

impl DocMock {
    fn new() -> Self {
        let mut rng = Rng(7);
        let events = (0..300u64)
            .map(|i| {
                let mut m = Map::new();
                m.insert("_id".into(), oid(i));
                m.insert("type".into(), J::String(["click", "view", "purchase", "signup"][(i % 4) as usize].into()));
                m.insert("at".into(), json!({"$date": {"$numberLong": (1_735_689_600_000u64 + i * 3_600_000).to_string()}}));
                let country = ["JP", "VN", "US"][(i % 3) as usize];
                m.insert("user".into(), json!({"id": {"$numberInt": (100 + rng.next(50)).to_string()}, "country": country}));
                if i % 4 == 2 {
                    m.insert("amount".into(), json!({"$numberDecimal": format!("{}.99", 5 + rng.next(90))}));
                }
                if i % 5 == 0 {
                    m.insert("tags".into(), json!(["mobile", "beta"]));
                }
                if i % 7 == 0 {
                    m.insert("note".into(), J::Null);
                }
                m.insert("count".into(), json!({"$numberLong": (rng.next(100_000)).to_string()}));
                m
            })
            .collect();
        let users = (0..40u64)
            .map(|i| {
                let mut m = Map::new();
                m.insert("_id".into(), oid(1000 + i));
                m.insert("name".into(), J::String(format!("user{i}")));
                m.insert("active".into(), J::Bool(i % 3 != 0));
                m.insert(
                    "score".into(),
                    json!({"$numberDouble": format!("{}.5", i * 3)}),
                );
                m
            })
            .collect();
        Self {
            colls: Mutex::new(vec![
                (ObjectRef::new("app", "events"), events),
                (ObjectRef::new("app", "users"), users),
            ]),
        }
    }
}

/// BSON-like JSON to a grid value, like the real MongoDB driver.
fn to_value(j: &J) -> Value {
    match j {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::String(s) => Value::Text(s.clone()),
        J::Object(m) if m.len() == 1 && m.contains_key("$numberInt") => Value::Int(
            m["$numberInt"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        ),
        J::Object(m) if m.len() == 1 && m.contains_key("$numberDouble") => Value::Float(
            m["$numberDouble"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0),
        ),
        other => Value::Json(other.clone()),
    }
}

#[async_trait::async_trait]
impl Driver for DocMock {
    fn kind(&self) -> Kind {
        Kind::MongoDb
    }
    async fn namespaces(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["admin".into(), "app".into()])
    }
    fn default_namespace(&self) -> Option<String> {
        Some("app".into())
    }
    async fn objects(&self, namespace: &str) -> anyhow::Result<Vec<ObjectInfo>> {
        let colls = self.colls.lock().unwrap();
        Ok(colls
            .iter()
            .filter(|(o, _)| o.namespace == namespace)
            .map(|(o, _)| ObjectInfo {
                name: o.name.clone(),
                kind: ObjectKind::Collection,
            })
            .collect())
    }
    async fn columns(&self, object: &ObjectRef) -> anyhow::Result<Vec<ColumnInfo>> {
        let colls = self.colls.lock().unwrap();
        let (_, docs) = colls
            .iter()
            .find(|(o, _)| o == object)
            .ok_or_else(|| anyhow!("no collection"))?;
        let mut names: Vec<String> = Vec::new();
        for d in docs.iter().take(100) {
            for k in d.keys() {
                if !names.contains(k) {
                    names.push(k.clone());
                }
            }
        }
        Ok(names
            .into_iter()
            .map(|n| ColumnInfo {
                key: n == "_id",
                type_name: if n == "_id" {
                    "objectId".into()
                } else {
                    "mixed".into()
                },
                name: n,
                nullable: true,
                default: None,
            })
            .collect())
    }
    async fn browse(&self, req: &BrowseRequest) -> anyhow::Result<Page> {
        tokio::time::sleep(Duration::from_millis(80)).await;
        let colls = self.colls.lock().unwrap();
        let (_, docs) = colls
            .iter()
            .find(|(o, _)| *o == req.object)
            .ok_or_else(|| anyhow!("no collection"))?;
        let page: Vec<&Map<String, J>> = docs
            .iter()
            .skip(req.offset as usize)
            .take(req.limit as usize)
            .collect();
        let mut columns: Vec<String> = vec!["_id".into()];
        for d in &page {
            for k in d.keys() {
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
        let rows = page
            .iter()
            .map(|d| {
                columns
                    .iter()
                    .map(|c| d.get(c).map(to_value).unwrap_or(Value::Missing))
                    .collect()
            })
            .collect();
        Ok(Page {
            result: ResultSet {
                types: vec![String::new(); columns.len()],
                columns,
                rows,
                ..Default::default()
            },
            key: vec!["_id".into()],
            read_only_reason: None,
        })
    }
    async fn count(&self, req: &BrowseRequest) -> anyhow::Result<u64> {
        let colls = self.colls.lock().unwrap();
        Ok(colls
            .iter()
            .find(|(o, _)| *o == req.object)
            .map(|(_, d)| d.len() as u64)
            .unwrap_or(0))
    }
    async fn execute(
        &self,
        _ns: Option<&str>,
        text: &str,
        _max: usize,
    ) -> anyhow::Result<Vec<ResultSet>> {
        let colls = self.colls.lock().unwrap();
        let name = text
            .trim()
            .strip_prefix("db.")
            .and_then(|r| r.split('.').next())
            .unwrap_or("");
        let (_, docs) = colls
            .iter()
            .find(|(o, _)| o.name == name)
            .ok_or_else(|| anyhow!("unknown collection \"{name}\""))?;
        let mut columns: Vec<String> = vec!["_id".into()];
        for d in docs.iter().take(20) {
            for k in d.keys() {
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
        let rows = docs
            .iter()
            .take(20)
            .map(|d| {
                columns
                    .iter()
                    .map(|c| d.get(c).map(to_value).unwrap_or(Value::Missing))
                    .collect()
            })
            .collect();
        Ok(vec![ResultSet {
            types: vec![String::new(); columns.len()],
            columns,
            rows,
            elapsed: Duration::from_millis(9),
            ..Default::default()
        }])
    }
    fn preview(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<String>> {
        let id =
            |key: &[(String, Value)]| key.first().map(|(_, v)| v.display()).unwrap_or_default();
        changes
            .iter()
            .map(|c| match c {
                Change::Update { key, set } => Ok(format!(
                    "db.{}.updateOne({{_id: {}}}, {{$set: {{{}}}}})",
                    object.name,
                    id(key),
                    set.iter()
                        .map(|(c, _, e)| match e {
                            Edit::Null => format!("{c}: null"),
                            Edit::Text(t) =>
                                format!("{c}: {}", serde_json::to_string(t).unwrap_or_default()),
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                Change::InsertDoc { json } => {
                    serde_json::from_str::<J>(json)
                        .map_err(|e| anyhow!("Invalid document: {e}"))?;
                    Ok(format!(
                        "db.{}.insertOne({})",
                        object.name,
                        json.split_whitespace().collect::<Vec<_>>().join(" ")
                    ))
                }
                Change::ReplaceDoc { key, json } => Ok(format!(
                    "db.{}.replaceOne({{_id: {}}}, {})",
                    object.name,
                    id(key),
                    json.split_whitespace().collect::<Vec<_>>().join(" ")
                )),
                Change::Delete { key } => Ok(format!(
                    "db.{}.deleteOne({{_id: {}}})",
                    object.name,
                    id(key)
                )),
                Change::Insert { .. } => bail!("use a document"),
            })
            .collect()
    }
    async fn apply(&self, _object: &ObjectRef, _changes: &[Change]) -> anyhow::Result<u64> {
        bail!("The demo document store is read-only.")
    }
}
