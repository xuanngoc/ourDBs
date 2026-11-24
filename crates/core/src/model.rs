use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{tunnel::Tunnel, value::Value};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Default row cap for query editor results.
pub const MAX_ROWS: usize = 10_000;

// ---------------------------------------------------------------------------
// Connection settings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Postgres,
    MySql,
    Sqlite,
    MongoDb,
    DocumentDb,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Postgres,
        Kind::MySql,
        Kind::Sqlite,
        Kind::MongoDb,
        Kind::DocumentDb,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Postgres => "PostgreSQL",
            Kind::MySql => "MySQL",
            Kind::Sqlite => "SQLite",
            Kind::MongoDb => "MongoDB",
            Kind::DocumentDb => "DocumentDB",
        }
    }

    /// Short badge text for the sidebar.
    pub fn short(self) -> &'static str {
        match self {
            Kind::Postgres => "PG",
            Kind::MySql => "MY",
            Kind::Sqlite => "SL",
            Kind::MongoDb => "MG",
            Kind::DocumentDb => "DD",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Kind::Postgres => 5432,
            Kind::MySql => 3306,
            Kind::Sqlite => 0,
            Kind::MongoDb | Kind::DocumentDb => 27017,
        }
    }

    pub fn default_tls(self) -> TlsMode {
        match self {
            Kind::Postgres | Kind::MySql => TlsMode::Prefer,
            Kind::Sqlite | Kind::MongoDb => TlsMode::Off,
            Kind::DocumentDb => TlsMode::Verify,
        }
    }

    /// MongoDB and DocumentDB: documents, collections, JSON filters.
    pub fn is_document(self) -> bool {
        matches!(self, Kind::MongoDb | Kind::DocumentDb)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TlsMode {
    /// Plain TCP.
    Off,
    /// TLS when the server offers it, no certificate checks; fall back to plain TCP.
    /// MongoDB/DocumentDB have no "prefer": they treat it as `Off`.
    #[default]
    Prefer,
    /// TLS required, no certificate checks (like libpq `sslmode=require`).
    Require,
    /// TLS required; verify the certificate chain and the host name.
    /// `ConnConfig::ca_file` adds a trusted CA (PEM).
    Verify,
}

impl TlsMode {
    pub const ALL: [TlsMode; 4] = [
        TlsMode::Off,
        TlsMode::Prefer,
        TlsMode::Require,
        TlsMode::Verify,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TlsMode::Off => "Off",
            TlsMode::Prefer => "Prefer",
            TlsMode::Require => "Require (no verify)",
            TlsMode::Verify => "Verify",
        }
    }
}

/// Environment tag. Shown as a colored dot and tab strip; `Prod` asks before writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Env {
    #[default]
    None,
    Dev,
    Staging,
    Prod,
}

impl Env {
    pub const ALL: [Env; 4] = [Env::None, Env::Dev, Env::Staging, Env::Prod];

    pub fn label(self) -> &'static str {
        match self {
            Env::None => "None",
            Env::Dev => "Dev",
            Env::Staging => "Staging",
            Env::Prod => "Prod",
        }
    }

    /// RGB color of the tag, `None` for no tag.
    pub fn rgb(self) -> Option<[u8; 3]> {
        match self {
            Env::None => None,
            Env::Dev => Some([46, 160, 67]),
            Env::Staging => Some([210, 153, 34]),
            Env::Prod => Some([218, 54, 51]),
        }
    }
}

/// One saved connection. Stored as JSON without the password; the password
/// lives in the macOS Keychain and is filled in just before `connect`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnConfig {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Empty means 127.0.0.1.
    pub host: String,
    /// 0 means `kind.default_port()`.
    pub port: u16,
    pub user: String,
    /// Never saved to disk.
    #[serde(skip)]
    pub password: String,
    /// PostgreSQL: database to connect to (empty = "postgres").
    /// MySQL: default schema (optional). MongoDB/DocumentDB: default database (optional).
    pub database: String,
    /// SQLite: path of the database file.
    pub path: String,
    /// Optional full URL (postgres://, mysql://, mongodb://, mongodb+srv://).
    /// When set, it replaces host/port/user/database. The password still comes
    /// from `password` when the URL has none.
    pub url: String,
    pub tls: TlsMode,
    /// PEM file with an extra trusted CA (for example the AWS global-bundle.pem).
    pub ca_file: String,
    /// SSH destination for a tunnel: "alias", "user@host" or "user@host:port".
    /// Empty means no tunnel. The tunnel uses the system `ssh` and ~/.ssh/config.
    pub ssh: String,
    /// Private key file for the SSH tunnel (for example ~/.ssh/id_ed25519).
    /// Empty means the ssh defaults (~/.ssh/config, the agent, default key names).
    pub ssh_key: String,
    /// The driver refuses writes (enforced by the server where possible).
    pub read_only: bool,
    pub env: Env,
    /// Set by the connector: host/port point at a local SSH tunnel port, so the
    /// TLS host name will not match the server certificate.
    #[serde(skip)]
    pub via_tunnel: bool,
}

impl ConnConfig {
    pub fn host_or_default(&self) -> &str {
        let h = self.host.trim();
        if h.is_empty() { "127.0.0.1" } else { h }
    }

    pub fn port_or_default(&self) -> u16 {
        if self.port == 0 {
            self.kind.default_port()
        } else {
            self.port
        }
    }

    pub fn has_url(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

// ---------------------------------------------------------------------------
// Schema objects
// ---------------------------------------------------------------------------

/// A table, view or collection. `namespace` is the PostgreSQL schema, the
/// MySQL database, the SQLite database name ("main") or the MongoDB database.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRef {
    pub namespace: String,
    pub name: String,
}

impl ObjectRef {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Table,
    View,
    Collection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    pub name: String,
    pub kind: ObjectKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// Database type name, for example "integer", "varchar(255)", "ObjectId".
    pub type_name: String,
    pub nullable: bool,
    /// Part of the primary key (or `_id` for documents).
    pub key: bool,
    pub default: Option<String>,
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultSet {
    pub columns: Vec<String>,
    /// Type name per column; an empty string when unknown. Same length as `columns`.
    pub types: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// Rows changed, for statements without a row result.
    pub affected: Option<u64>,
    /// Extra text for the status line, for example "inserted 1 document".
    pub message: Option<String>,
    /// True when more rows existed than the row cap.
    pub truncated: bool,
    pub elapsed: Duration,
}

/// Request for one page of a table or collection.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowseRequest {
    pub object: ObjectRef,
    /// SQL: the body of a WHERE clause. Documents: a filter in mongosh syntax
    /// (`{age: {$gt: 30}}`, `ObjectId("...")`, ...). Empty = no filter.
    pub filter: String,
    /// (column, ascending). `None`: order by the key columns when there are any.
    pub sort: Option<(String, bool)>,
    pub limit: u32,
    pub offset: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub result: ResultSet,
    /// Key columns (primary key or `_id`). Empty means the grid is read-only.
    pub key: Vec<String>,
    /// Why the page cannot be edited ("no primary key", "view", "read-only connection").
    pub read_only_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Edits
// ---------------------------------------------------------------------------

/// Raw input from the user for one cell. The driver turns it into a typed value.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Null,
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Change cells of one row. `key` has (column, current value) for every key
    /// column. `set` has (column, original value, new input).
    Update {
        key: Vec<(String, Value)>,
        set: Vec<(String, Value, Edit)>,
    },
    /// Insert one row. Columns not listed get their default value.
    Insert { values: Vec<(String, Edit)> },
    /// Delete one row.
    Delete { key: Vec<(String, Value)> },
    /// Documents only: insert a whole document (mongosh-style JSON text).
    InsertDoc { json: String },
    /// Documents only: replace the document with this `_id` (key) by `json`.
    ReplaceDoc {
        key: Vec<(String, Value)>,
        json: String,
    },
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait Driver: Send + Sync {
    fn kind(&self) -> Kind;

    /// PostgreSQL only: databases on the server. The UI reconnects to switch.
    async fn databases(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// PostgreSQL only: the database this session is connected to.
    fn current_database(&self) -> Option<String> {
        None
    }

    /// Schemas (PostgreSQL), databases (MySQL, MongoDB) or attached databases (SQLite).
    async fn namespaces(&self) -> anyhow::Result<Vec<String>>;

    /// Namespace to open first: "public", the configured database, "main".
    fn default_namespace(&self) -> Option<String>;

    /// Tables, views and collections, sorted by name.
    async fn objects(&self, namespace: &str) -> anyhow::Result<Vec<ObjectInfo>>;

    /// Columns of a table. Documents: fields from a sample of documents.
    async fn columns(&self, object: &ObjectRef) -> anyhow::Result<Vec<ColumnInfo>>;

    /// One page of rows.
    async fn browse(&self, req: &BrowseRequest) -> anyhow::Result<Page>;

    /// Number of rows that match `req.filter` (limit/offset/sort are ignored).
    async fn count(&self, req: &BrowseRequest) -> anyhow::Result<u64>;

    /// Run query-editor text. `namespace` is the schema/database to run in.
    /// Returns one `ResultSet` per statement. At most `max_rows` rows per result.
    async fn execute(
        &self,
        namespace: Option<&str>,
        text: &str,
        max_rows: usize,
    ) -> anyhow::Result<Vec<ResultSet>>;

    /// Best-effort stop of a running `execute`.
    async fn cancel(&self) {}

    /// The statements `apply` will run, as readable text (SQL or mongosh).
    fn preview(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<Vec<String>>;

    /// Apply changes. SQL: in one transaction, all or nothing. Documents: in
    /// order, stop at the first error. Each update/delete must hit exactly one
    /// row. Returns the number of changes applied.
    async fn apply(&self, object: &ObjectRef, changes: &[Change]) -> anyhow::Result<u64>;

    async fn close(&self) {}
}

/// An open connection. Keep the tunnel alive as long as the driver.
pub struct Session {
    pub driver: Arc<dyn Driver>,
    pub tunnel: Option<Tunnel>,
}

/// Opens a [`Session`] for a config (the password is already filled in).
pub type Connector =
    Arc<dyn Fn(ConnConfig) -> BoxFuture<'static, anyhow::Result<Session>> + Send + Sync>;
