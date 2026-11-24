//! OurDB driver for MongoDB and Amazon DocumentDB (SPEC sections 4, 5 and 6).
//!
//! One `mongodb::Client` serves browse, metadata and the query editor. The
//! client has a connection pool, so a long query does not block the sidebar.

mod convert;
mod shell;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use futures_util::TryStreamExt;
use mongodb::{
    Client, Collection, Cursor, Database,
    bson::{Bson, Document, doc},
    error::{Error as MongoError, ErrorKind, WriteFailure},
    options::{
        AuthMechanism, ClientOptions, ConnectionString, Credential, ServerAddress, Tls,
        UpdateModifications,
    },
    results::UpdateResult,
};
use ourdb_core::{
    BrowseRequest, Change, ColumnInfo, ConnConfig, Driver, Edit, Kind, ObjectInfo, ObjectKind,
    ObjectRef, Page, ResultSet, TlsMode, Value,
};

use crate::{
    convert::{
        coll_text, docs_result, from_value, shell_doc, shell_text, typed_edit, union_columns,
    },
    shell::{Call, Stmt, Update},
};

/// Connect and server selection timeout (SPEC section 4).
const TIMEOUT: Duration = Duration::from_secs(10);

/// CA bundle for DocumentDB with TLS Verify (SPEC section 5).
const AWS_CA_URL: &str = "https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem";

/// Open a connection. The password is already in `cfg.password`.
pub async fn connect(cfg: &ConnConfig) -> anyhow::Result<Arc<dyn Driver>> {
    let opts = client_options(cfg).await?;
    let default_db = opts
        .default_database
        .clone()
        .or_else(|| non_empty(&cfg.database));
    let client = Client::with_options(opts).map_err(mongo_error)?;
    // The client connects lazily: a ping shows connection and login errors now.
    client
        .database("admin")
        .run_command(doc! {"ping": 1})
        .await
        .map_err(mongo_error)?;
    Ok(Arc::new(MongoDriver {
        kind: cfg.kind,
        client,
        default_db,
        read_only: cfg.read_only,
    }))
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Client options from the URL or from the fields of the config.
async fn client_options(cfg: &ConnConfig) -> Result<ClientOptions> {
    let mut o = if cfg.has_url() {
        let url = cfg.url.trim();
        if cfg.via_tunnel && url.starts_with("mongodb+srv://") {
            bail!(
                "a mongodb+srv:// URL cannot go through an SSH tunnel: use a mongodb:// URL with the host and port of one server"
            );
        }
        let conn = ConnectionString::parse(url).map_err(mongo_error)?;
        let url_source = conn.credential.as_ref().is_some_and(|c| c.source.is_some());
        let mut o = ClientOptions::parse(conn).await.map_err(mongo_error)?;
        if let Some(c) = o.credential.as_mut() {
            if c.password.is_none() && !cfg.password.is_empty() {
                c.password = Some(cfg.password.clone());
            }
            // The driver takes the URL path as the source; we want "admin".
            if !url_source {
                c.source = None;
            }
        }
        o
    } else {
        let mut o = ClientOptions::default();
        o.hosts = vec![ServerAddress::Tcp {
            host: cfg.host_or_default().to_string(),
            port: Some(cfg.port_or_default()),
        }];
        if let Some(user) = non_empty(&cfg.user) {
            let mut c = Credential::default();
            c.username = Some(user);
            c.password = (!cfg.password.is_empty()).then(|| cfg.password.clone());
            o.credential = Some(c);
        }
        o.default_database = non_empty(&cfg.database);
        o
    };
    // Credential source "admin" unless the URL names one (authSource).
    if let Some(c) = o.credential.as_mut()
        && c.source.is_none()
        && matches!(
            c.mechanism,
            None | Some(AuthMechanism::ScramSha1 | AuthMechanism::ScramSha256)
        )
    {
        c.source = Some("admin".into());
    }
    o.app_name = Some("OurDB".into());
    o.connect_timeout = Some(TIMEOUT);
    o.server_selection_timeout = Some(TIMEOUT);
    if cfg.kind == Kind::DocumentDb {
        o.retry_writes = Some(false);
    }
    if cfg.via_tunnel {
        // The tunnel goes to one server: talk to it directly.
        o.hosts = vec![ServerAddress::Tcp {
            host: cfg.host_or_default().to_string(),
            port: Some(cfg.port_or_default()),
        }];
        o.direct_connection = Some(true);
    }
    o.tls = Some(tls_options(cfg, o.tls.take()).await?);
    Ok(o)
}

/// TLS from the TLS mode. Off/Prefer: no TLS, unless the URL turns TLS on
/// (`tls=true`, `mongodb+srv://`). Require: no certificate checks. Verify:
/// check the chain, with the CA file when there is one.
async fn tls_options(cfg: &ConnConfig, from_url: Option<Tls>) -> Result<Tls> {
    let url_tls = match from_url {
        Some(Tls::Enabled(t)) => Some(t),
        _ => None,
    };
    let mut t = match cfg.tls {
        TlsMode::Off | TlsMode::Prefer => match url_tls {
            Some(t) => t,
            None => return Ok(Tls::Disabled),
        },
        TlsMode::Require => {
            let mut t = url_tls.unwrap_or_default();
            t.allow_invalid_certificates = Some(true);
            t
        }
        TlsMode::Verify => {
            let mut t = url_tls.unwrap_or_default();
            if let Some(ca) = non_empty(&cfg.ca_file) {
                t.ca_file_path = Some(expand_home(&ca));
            } else if cfg.kind == Kind::DocumentDb && t.ca_file_path.is_none() && !cfg.via_tunnel {
                t.ca_file_path = Some(aws_ca_bundle().await?);
            }
            t
        }
    };
    if cfg.via_tunnel {
        // The host name is 127.0.0.1, so it never matches the certificate, and
        // rustls cannot skip only the host name check.
        t.allow_invalid_certificates = Some(true);
    }
    Ok(Tls::Enabled(t))
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// The AWS CA bundle for DocumentDB. We download it once with the system
/// `curl` and keep it in the OurDB support folder.
async fn aws_ca_bundle() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    let path = Path::new(&home).join("Library/Application Support/OurDB/rds-global-bundle.pem");
    if path.is_file() {
        return Ok(path);
    }
    let target = path.clone();
    let result = tokio::task::spawn_blocking(move || download(AWS_CA_URL, &target)).await?;
    result.map_err(|e| {
        anyhow!("DocumentDB with TLS Verify needs the AWS CA bundle, and the download failed ({e}). Download {AWS_CA_URL} and set it as the CA file.")
    })?;
    Ok(path)
}

fn download(url: &str, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = path.with_extension("pem.part");
    let out = std::process::Command::new("/usr/bin/curl")
        .args(["-fsSL", "--max-time", "15", "-o"])
        .arg(&part)
        .arg(url)
        .output()?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    std::fs::rename(&part, path)?;
    Ok(())
}

/// A readable message for a driver error: the server message without the
/// Rust debug text.
fn mongo_error(e: MongoError) -> anyhow::Error {
    let named = |message: &str, code_name: Option<&str>| match code_name.filter(|n| !n.is_empty()) {
        Some(n) => format!("{message} ({n})"),
        None => message.to_string(),
    };
    let text = match &*e.kind {
        ErrorKind::Command(c) => named(&c.message, Some(&c.code_name)),
        ErrorKind::Write(WriteFailure::WriteError(w)) => named(&w.message, w.code_name.as_deref()),
        ErrorKind::Write(WriteFailure::WriteConcernError(w)) => format!(
            "write concern error: {}",
            named(&w.message, Some(&w.code_name))
        ),
        ErrorKind::InsertMany(m) => match (&m.write_errors, &m.write_concern_error) {
            (Some(errs), _) if !errs.is_empty() => {
                let first = &errs[0];
                let more = if errs.len() > 1 {
                    format!(" (and {} more errors)", errs.len() - 1)
                } else {
                    String::new()
                };
                format!(
                    "document {}: {}{more}",
                    first.index + 1,
                    named(&first.message, first.code_name.as_deref())
                )
            }
            (_, Some(w)) => format!(
                "write concern error: {}",
                named(&w.message, Some(&w.code_name))
            ),
            _ => "insertMany failed".to_string(),
        },
        ErrorKind::Authentication { message, .. } => format!("authentication failed: {message}"),
        ErrorKind::ServerSelection { message, .. } => {
            format!("cannot reach the server: {}", clean_selection(message))
        }
        ErrorKind::InvalidArgument { message, .. } => message.clone(),
        other => other.to_string(),
    };
    anyhow!(text)
}

/// Server selection messages list the topology with the driver's debug text.
/// Keep the first server error when there is one.
fn clean_selection(message: &str) -> String {
    if let Some(p) = message.find("Error: Kind: ") {
        let rest = &message[p + 13..];
        let end = rest.find(", labels:").unwrap_or(rest.len());
        return rest[..end].to_string();
    }
    message.to_string()
}

struct MongoDriver {
    kind: Kind,
    client: Client,
    /// The configured database (field or URL path).
    default_db: Option<String>,
    read_only: bool,
}

impl MongoDriver {
    fn coll(&self, object: &ObjectRef) -> Collection<Document> {
        self.client
            .database(&object.namespace)
            .collection(&object.name)
    }

    /// The database for the query editor: the picked one, else the
    /// configured one, else "test" (like mongosh).
    fn db(&self, namespace: Option<&str>) -> Database {
        let name = namespace
            .filter(|n| !n.trim().is_empty())
            .or(self.default_db.as_deref())
            .unwrap_or("test");
        self.client.database(name)
    }

    fn check_writable(&self) -> Result<()> {
        if self.read_only {
            bail!("read-only connection");
        }
        Ok(())
    }

    /// Collections and views of a database, without `system.*`.
    async fn list_collections(&self, db: &Database) -> Result<Vec<ObjectInfo>> {
        let cursor = db
            .run_cursor_command(doc! {"listCollections": 1, "nameOnly": true})
            .await
            .map_err(mongo_error)?;
        let docs = take_docs(cursor, usize::MAX).await?;
        let mut out: Vec<ObjectInfo> = docs
            .iter()
            .filter_map(|d| {
                let name = d.get_str("name").ok()?;
                let kind = if d.get_str("type").is_ok_and(|t| t == "view") {
                    ObjectKind::View
                } else {
                    ObjectKind::Collection
                };
                (!name.starts_with("system.")).then(|| ObjectInfo {
                    name: name.to_string(),
                    kind,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// True when the object is a view. Errors count as "not a view": the
    /// server refuses the write later if it is one.
    async fn is_view(&self, object: &ObjectRef) -> bool {
        let db = self.client.database(&object.namespace);
        let cmd = doc! {"listCollections": 1, "nameOnly": true, "filter": {"name": &object.name}};
        match db.run_cursor_command(cmd).await {
            Ok(cursor) => take_docs(cursor, 1).await.is_ok_and(|docs| {
                docs.first()
                    .is_some_and(|d| d.get_str("type").is_ok_and(|t| t == "view"))
            }),
            Err(_) => false,
        }
    }

    async fn run_statement(&self, db: &Database, stmt: Stmt, max_rows: usize) -> Result<ResultSet> {
        match stmt {
            Stmt::ShowDbs => {
                let dbs = self.client.list_databases().await.map_err(mongo_error)?;
                let rows = dbs
                    .into_iter()
                    .map(|d| {
                        vec![
                            Value::Text(d.name),
                            Value::Int(i64::try_from(d.size_on_disk).unwrap_or(i64::MAX)),
                            Value::Bool(d.empty),
                        ]
                    })
                    .collect();
                Ok(table(
                    &["name", "sizeOnDisk", "empty"],
                    &["String", "Int64", "Boolean"],
                    rows,
                ))
            }
            Stmt::ShowCollections => {
                let rows = self
                    .list_collections(db)
                    .await?
                    .into_iter()
                    .map(|o| vec![Value::Text(o.name)])
                    .collect();
                Ok(table(&["name"], &["String"], rows))
            }
            Stmt::RunCommand(cmd) => {
                let reply = db.run_command(cmd).await.map_err(mongo_error)?;
                let docs = [reply];
                Ok(docs_result(&docs, union_columns(&docs)))
            }
            Stmt::Coll { name, call } => run_call(&db.collection(&name), call, max_rows).await,
        }
    }
}

/// A result table with fixed columns.
fn table(columns: &[&str], types: &[&str], rows: Vec<Vec<Value>>) -> ResultSet {
    ResultSet {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        types: types.iter().map(|t| t.to_string()).collect(),
        rows,
        ..ResultSet::default()
    }
}

/// Read at most `n` documents. Dropping the cursor closes it on the server.
async fn take_docs(mut cursor: Cursor<Document>, n: usize) -> Result<Vec<Document>> {
    let mut docs = Vec::new();
    while docs.len() < n {
        match cursor.try_next().await.map_err(mongo_error)? {
            Some(d) => docs.push(d),
            None => break,
        }
    }
    Ok(docs)
}

/// Documents as a grid, cut at `max_rows`.
fn docs_rows(mut docs: Vec<Document>, max_rows: usize) -> ResultSet {
    let truncated = docs.len() > max_rows;
    docs.truncate(max_rows);
    let mut rs = docs_result(&docs, union_columns(&docs));
    rs.truncated = truncated;
    rs
}

fn count_result(n: u64) -> ResultSet {
    table(
        &["count"],
        &["Int64"],
        vec![vec![Value::Int(i64::try_from(n).unwrap_or(i64::MAX))]],
    )
}

fn write_result(affected: u64, message: String) -> ResultSet {
    ResultSet {
        affected: Some(affected),
        message: Some(message),
        ..ResultSet::default()
    }
}

fn update_result(r: UpdateResult) -> ResultSet {
    let mut message = format!("matched {}, modified {}", r.matched_count, r.modified_count);
    let mut affected = r.modified_count;
    if let Some(id) = &r.upserted_id {
        message.push_str(&format!(", upserted _id {}", shell_text(id)));
        affected += 1;
    }
    write_result(affected, message)
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Run one collection method of the query editor.
async fn run_call(coll: &Collection<Document>, call: Call, max_rows: usize) -> Result<ResultSet> {
    // One row more than the cap shows that more rows exist.
    let cap = max_rows.saturating_add(1);
    Ok(match call {
        Call::Find {
            filter,
            projection,
            sort,
            skip,
            limit,
        } => {
            let cap_i64 = i64::try_from(cap).unwrap_or(i64::MAX);
            let limit = match limit {
                Some(l) if l != 0 => l.unsigned_abs().min(cap_i64 as u64) as i64,
                _ => cap_i64,
            };
            let mut find = coll.find(filter).limit(limit);
            if let Some(p) = projection {
                find = find.projection(p);
            }
            if let Some(s) = sort {
                find = find.sort(s);
            }
            if let Some(s) = skip {
                find = find.skip(s);
            }
            let cursor = find.await.map_err(mongo_error)?;
            docs_rows(take_docs(cursor, cap).await?, max_rows)
        }
        Call::FindOne { filter, projection } => {
            let mut find = coll.find_one(filter);
            if let Some(p) = projection {
                find = find.projection(p);
            }
            let doc = find.await.map_err(mongo_error)?;
            docs_rows(doc.into_iter().collect(), max_rows)
        }
        Call::Aggregate(pipeline) => {
            let cursor = coll.aggregate(pipeline).await.map_err(mongo_error)?;
            docs_rows(take_docs(cursor, cap).await?, max_rows)
        }
        Call::CountDocuments(filter) => {
            count_result(coll.count_documents(filter).await.map_err(mongo_error)?)
        }
        Call::EstimatedDocumentCount => {
            count_result(coll.estimated_document_count().await.map_err(mongo_error)?)
        }
        Call::Distinct { field, filter } => {
            let values = coll.distinct(&field, filter).await.map_err(mongo_error)?;
            let truncated = values.len() > max_rows;
            let docs: Vec<Document> = values
                .into_iter()
                .take(max_rows)
                .map(|v| doc! {field.as_str(): v})
                .collect();
            let mut rs = docs_result(&docs, vec![field]);
            rs.truncated = truncated;
            rs
        }
        Call::InsertOne(doc) => {
            let r = coll.insert_one(doc).await.map_err(mongo_error)?;
            write_result(
                1,
                format!("inserted 1 document, _id {}", shell_text(&r.inserted_id)),
            )
        }
        Call::InsertMany(docs) => {
            let r = coll.insert_many(docs).await.map_err(mongo_error)?;
            let n = r.inserted_ids.len() as u64;
            write_result(n, format!("inserted {}", plural(n, "document")))
        }
        Call::UpdateOne {
            filter,
            update,
            upsert,
        } => update_result(
            coll.update_one(filter, modifications(update))
                .upsert(upsert)
                .await
                .map_err(mongo_error)?,
        ),
        Call::UpdateMany {
            filter,
            update,
            upsert,
        } => update_result(
            coll.update_many(filter, modifications(update))
                .upsert(upsert)
                .await
                .map_err(mongo_error)?,
        ),
        Call::ReplaceOne {
            filter,
            doc,
            upsert,
        } => update_result(
            coll.replace_one(filter, doc)
                .upsert(upsert)
                .await
                .map_err(mongo_error)?,
        ),
        Call::DeleteOne(filter) => {
            let n = coll
                .delete_one(filter)
                .await
                .map_err(mongo_error)?
                .deleted_count;
            write_result(n, format!("deleted {}", plural(n, "document")))
        }
        Call::DeleteMany(filter) => {
            let n = coll
                .delete_many(filter)
                .await
                .map_err(mongo_error)?
                .deleted_count;
            write_result(n, format!("deleted {}", plural(n, "document")))
        }
    })
}

fn modifications(u: Update) -> UpdateModifications {
    match u {
        Update::Doc(d) => UpdateModifications::Document(d),
        Update::Pipeline(p) => UpdateModifications::Pipeline(p),
    }
}

// ---------------------------------------------------------------------------
// Staged edits
// ---------------------------------------------------------------------------

/// One write of `apply()`. `preview()` prints the same list.
enum Op {
    Update { filter: Document, set: Document },
    Insert(Document),
    Delete(Document),
    Replace { filter: Document, doc: Document },
}

impl Op {
    /// mongosh text of the write.
    fn text(&self, coll: &str) -> String {
        let c = coll_text(coll);
        match self {
            Op::Update { filter, set } => format!(
                "{c}.updateOne({}, {{$set: {}}})",
                shell_doc(filter),
                shell_doc(set)
            ),
            Op::Insert(doc) => format!("{c}.insertOne({})", shell_doc(doc)),
            Op::Delete(filter) => format!("{c}.deleteOne({})", shell_doc(filter)),
            Op::Replace { filter, doc } => {
                format!("{c}.replaceOne({}, {})", shell_doc(filter), shell_doc(doc))
            }
        }
    }

    /// Run the write. Updates, replaces and deletes must hit exactly one document.
    async fn run(&self, coll: &Collection<Document>) -> Result<()> {
        let exactly_one = |n: u64, what: &str| {
            if n == 1 {
                Ok(())
            } else {
                Err(anyhow!(
                    "{what} {}, expected exactly 1",
                    plural(n, "document")
                ))
            }
        };
        match self {
            Op::Update { filter, set } => {
                let r = coll
                    .update_one(filter.clone(), doc! {"$set": set.clone()})
                    .await
                    .map_err(mongo_error)?;
                exactly_one(r.matched_count, "matched")
            }
            Op::Insert(doc) => coll.insert_one(doc).await.map(|_| ()).map_err(mongo_error),
            Op::Delete(filter) => {
                let r = coll.delete_one(filter.clone()).await.map_err(mongo_error)?;
                exactly_one(r.deleted_count, "deleted")
            }
            Op::Replace { filter, doc } => {
                let r = coll
                    .replace_one(filter.clone(), doc)
                    .await
                    .map_err(mongo_error)?;
                exactly_one(r.matched_count, "matched")
            }
        }
    }
}

/// The filter for one row: `{_id: …}` from the key values.
fn key_filter(key: &[(String, Value)]) -> Result<Document> {
    if key.is_empty() {
        bail!("the row has no key: documents need an _id");
    }
    let mut filter = Document::new();
    for (col, v) in key {
        let b = from_value(v).map_err(|e| anyhow!("key {col}: {e}"))?;
        filter.insert(col.clone(), b);
    }
    Ok(filter)
}

/// Turn the staged changes into writes. All input is checked before the
/// first write.
fn plan(changes: &[Change]) -> Result<Vec<Op>> {
    let one = |ch: &Change| -> Result<Op> {
        Ok(match ch {
            Change::Update { key, set } => {
                if set.is_empty() {
                    bail!("nothing to update");
                }
                let mut doc = Document::new();
                for (col, original, edit) in set {
                    doc.insert(
                        col.clone(),
                        typed_edit(original, edit).map_err(|e| anyhow!("{col}: {e}"))?,
                    );
                }
                Op::Update {
                    filter: key_filter(key)?,
                    set: doc,
                }
            }
            Change::Insert { values } => {
                let mut doc = Document::new();
                for (col, edit) in values {
                    // An empty _id means "let the server make one".
                    if col == "_id" && matches!(edit, Edit::Text(t) if t.trim().is_empty()) {
                        continue;
                    }
                    doc.insert(
                        col.clone(),
                        typed_edit(&Value::Null, edit).map_err(|e| anyhow!("{col}: {e}"))?,
                    );
                }
                Op::Insert(doc)
            }
            Change::Delete { key } => Op::Delete(key_filter(key)?),
            Change::InsertDoc { json } => Op::Insert(shell::parse_doc(json)?),
            Change::ReplaceDoc { key, json } => {
                let filter = key_filter(key)?;
                let doc = shell::parse_doc(json)?;
                if let (Some(new), Some(old)) = (doc.get("_id"), filter.get("_id"))
                    && new != old
                {
                    bail!(
                        "the _id cannot change (from {} to {})",
                        shell_text(old),
                        shell_text(new)
                    );
                }
                Op::Replace { filter, doc }
            }
        })
    };
    changes
        .iter()
        .enumerate()
        .map(|(i, ch)| one(ch).map_err(|e| anyhow!("change {}: {e}", i + 1)))
        .collect()
}

#[async_trait::async_trait]
impl Driver for MongoDriver {
    fn kind(&self) -> Kind {
        self.kind
    }

    async fn namespaces(&self) -> Result<Vec<String>> {
        match self.client.list_database_names().await {
            Ok(mut names) => {
                if let Some(db) = &self.default_db
                    && !names.contains(db)
                {
                    names.push(db.clone());
                }
                names.sort();
                Ok(names)
            }
            // A user without the listDatabases right still sees the configured database.
            Err(e) if matches!(*e.kind, ErrorKind::Command(_)) && self.default_db.is_some() => {
                Ok(self.default_db.iter().cloned().collect())
            }
            Err(e) => Err(mongo_error(e)),
        }
    }

    fn default_namespace(&self) -> Option<String> {
        self.default_db.clone()
    }

    async fn objects(&self, namespace: &str) -> Result<Vec<ObjectInfo>> {
        self.list_collections(&self.client.database(namespace))
            .await
    }

    async fn columns(&self, object: &ObjectRef) -> Result<Vec<ColumnInfo>> {
        let cursor = self
            .coll(object)
            .find(doc! {})
            .limit(100)
            .await
            .map_err(mongo_error)?;
        let docs = take_docs(cursor, 100).await?;
        let mut names = union_columns(&docs);
        if names.first().map(String::as_str) != Some("_id") {
            names.insert(0, "_id".to_string());
        }
        let types = convert::column_types(&docs, &names);
        Ok(names
            .into_iter()
            .zip(types)
            .map(|(name, type_name)| {
                let key = name == "_id";
                let type_name = if type_name.is_empty() && key {
                    "ObjectId".to_string()
                } else {
                    type_name
                };
                let nullable = !key
                    && docs
                        .iter()
                        .any(|d| matches!(d.get(&name), None | Some(Bson::Null)));
                ColumnInfo {
                    name,
                    type_name,
                    nullable,
                    key,
                    default: None,
                }
            })
            .collect())
    }

    async fn browse(&self, req: &BrowseRequest) -> Result<Page> {
        let started = Instant::now();
        let filter = shell::parse_doc(&req.filter).map_err(|e| anyhow!("filter: {e}"))?;
        // Always sort: DocumentDB has no natural order. `_id` breaks ties, so
        // pages are stable.
        let sort = match &req.sort {
            Some((col, asc)) if col != "_id" => {
                doc! {col.as_str(): if *asc { 1 } else { -1 }, "_id": 1}
            }
            Some((_, asc)) => doc! {"_id": if *asc { 1 } else { -1 }},
            None => doc! {"_id": 1},
        };
        let limit = req.limit.max(1);
        let find = async {
            let cursor = self
                .coll(&req.object)
                .find(filter)
                .sort(sort)
                .skip(req.offset)
                .limit(i64::from(limit))
                .batch_size(limit)
                .await
                .map_err(mongo_error)?;
            take_docs(cursor, limit as usize).await
        };
        let (docs, view) = tokio::join!(find, self.is_view(&req.object));
        let docs = docs?;
        let mut columns = union_columns(&docs);
        if columns.first().map(String::as_str) != Some("_id") {
            columns.insert(0, "_id".to_string());
        }
        let mut result = docs_result(&docs, columns);
        result.elapsed = started.elapsed();
        let reason = if self.read_only {
            Some("read-only connection")
        } else if view {
            Some("view")
        } else {
            None
        };
        Ok(Page {
            result,
            key: if reason.is_none() {
                vec!["_id".to_string()]
            } else {
                Vec::new()
            },
            read_only_reason: reason.map(str::to_string),
        })
    }

    async fn count(&self, req: &BrowseRequest) -> Result<u64> {
        let filter = shell::parse_doc(&req.filter).map_err(|e| anyhow!("filter: {e}"))?;
        self.coll(&req.object)
            .count_documents(filter)
            .await
            .map_err(mongo_error)
    }

    async fn execute(
        &self,
        namespace: Option<&str>,
        text: &str,
        max_rows: usize,
    ) -> Result<Vec<ResultSet>> {
        // Parse every statement first, so a typo in the last one runs nothing.
        let stmts = shell::split_statements(text)
            .iter()
            .enumerate()
            .map(|(i, s)| {
                shell::parse_statement(s).map_err(|e| anyhow!("statement {}: {e}", i + 1))
            })
            .collect::<Result<Vec<_>>>()?;
        if self.read_only
            && let Some(i) = stmts.iter().position(Stmt::is_write)
        {
            bail!("statement {}: read-only connection", i + 1);
        }
        let db = self.db(namespace);
        let mut out = Vec::with_capacity(stmts.len());
        for (i, stmt) in stmts.into_iter().enumerate() {
            let started = Instant::now();
            let mut rs = self
                .run_statement(&db, stmt, max_rows)
                .await
                .map_err(|e| anyhow!("statement {}: {e}", i + 1))?;
            rs.elapsed = started.elapsed();
            out.push(rs);
        }
        Ok(out)
    }

    fn preview(&self, object: &ObjectRef, changes: &[Change]) -> Result<Vec<String>> {
        Ok(plan(changes)?
            .iter()
            .map(|op| op.text(&object.name))
            .collect())
    }

    async fn apply(&self, object: &ObjectRef, changes: &[Change]) -> Result<u64> {
        self.check_writable()?;
        let ops = plan(changes)?;
        let coll = self.coll(object);
        for (i, op) in ops.iter().enumerate() {
            if let Err(e) = op.run(&coll).await {
                let done = match i {
                    0 => String::new(),
                    1 => " Change 1 was applied.".to_string(),
                    _ => format!(" Changes 1 to {i} were applied."),
                };
                let why = e.to_string();
                let why = why.trim_end_matches('.');
                bail!(
                    "change {} of {} failed: {why}.{done}\n{}",
                    i + 1,
                    ops.len(),
                    shell::cut(&op.text(&object.name), 300)
                );
            }
        }
        Ok(ops.len() as u64)
    }

    async fn close(&self) {
        self.client.clone().shutdown().immediate(true).await;
    }
}
