# OurDB specification (v0.1)

OurDB is a small database client for macOS. It shows data as a table and lets you edit it. It supports PostgreSQL, MySQL, SQLite, MongoDB and Amazon DocumentDB. It is one native Rust binary (egui/eframe) in a `.app` bundle.

The main rule is **simple and correct**. A small feature set that works every time is better than many features.

## 1. Research summary

| Tool | Users like | Users dislike | What OurDB takes |
|---|---|---|---|
| TablePlus | Fast native UI, staged edits with SQL review, safe mode | Paid; free tier has 2 tabs | Staged edits + review dialog, env color tags |
| Beekeeper Studio | Clean UI, read-only connections | Slow on big data, no MongoDB | Read-only mode |
| Sequel Ace | Free, native | MySQL only | Native single app |
| DbGate | SQL + NoSQL in one tool | Cluttered UI | One UI for SQL and documents, but less clutter |
| MongoDB Compass | All MongoDB features | Slow start, freezes, fails on DocumentDB | Fast grid, DocumentDB works |
| Harlequin, rainfrog, gobang | Fast, Keychain passwords | Terminal only, stale (gobang) | Keychain passwords, query history |

**Cut from v1:** ER diagrams, schema designer, import, dump/restore, data compare, user admin, explain view, aggregation builder, AI, cloud sync, plugins, charts, client certificates, autocomplete.

## 2. Features (v1)

1. **Connections.** A form with name, type, host, port, user, password, database, SQLite file, optional URL, TLS mode, CA file, SSH tunnel, read-only flag and env tag. A "Test" button. Passwords go to the macOS Keychain. Everything else goes to `~/Library/Application Support/OurDB/connections.json`.
2. **SSH tunnel.** OurDB runs the system `ssh -N -L` (it uses `~/.ssh/config`, keys and the agent). Password prompts are not supported.
3. **Sidebar tree.** Connection → namespace → tables, views and collections. A filter box and refresh. PostgreSQL also has a database picker.
4. **Data grid.** Server-side pages (50/100/500/1000 rows), virtual scroll, resizable columns. NULL looks different from an empty string. A missing document field looks different from null.
5. **Sort and filter.** Click a header to sort. A filter box takes a SQL WHERE body, or a JSON filter for documents. "Count" shows the total on request.
6. **Value panel.** It shows the full value of the selected cell (pretty JSON). You can edit it there.
7. **Staged edits.** Edit cells, add rows and delete rows. Changes stay local and are highlighted. "Save" shows the exact statements, then applies them. SQL changes run in one transaction. Edits need a primary key (or `_id`).
8. **Query editor.** Run all text or only the selection (⌘↵). It shows many result sets, row count and time. It has Stop, history, and CSV/JSON export.
9. **Read-only mode,** enforced by the server where possible.
10. **Confirm** before DROP, TRUNCATE, ALTER … DROP, DELETE/UPDATE without WHERE, `deleteMany({})` and `drop()`. On a `Prod` connection, confirm every write.
11. **Env tag** (Dev, Staging, Prod) as a colored dot and a tab strip.
12. **Export** of a result to CSV or JSON. Copy a cell, a row as TSV, or a row as JSON.

## 3. Architecture

```
crates/
  core/      ourdb-core      Shared types, Driver trait, Value, SQL quoting, export, SSH tunnel
  postgres/  ourdb-postgres  tokio-postgres + postgres-native-tls
  mysql/     ourdb-mysql     mysql_async (native-tls)
  sqlite/    ourdb-sqlite    rusqlite (bundled SQLite)
  mongo/     ourdb-mongo     mongodb 3.x + bson 3 (MongoDB and DocumentDB)
  ui/        ourdb-ui        egui/eframe app, connection store, Keychain
  app/       ourdb           Binary: wires the UI to the drivers (main.rs)
```

- The contract is in `crates/core/src/model.rs` (`Driver`, `ConnConfig`, `Change`, `ResultSet`, ...) and `crates/core/src/value.rs` (`Value`). Read these two files first.
- Each driver crate exports exactly one entry point: `pub async fn connect(cfg: &ConnConfig) -> anyhow::Result<Arc<dyn Driver>>`.
- The UI gets a `Connector` and never names a driver. The binary opens the SSH tunnel and then calls the driver. With a tunnel, `cfg.host` is 127.0.0.1, `cfg.port` is the local port and `cfg.via_tunnel` is true.
- The UI thread never waits for the database. All driver calls run on a tokio runtime. Results come back over a channel.

## 4. Driver rules (all drivers)

- **Values.** SQL drivers return `Value::Null` or `Value::Text` with the server's text form. MySQL and SQLite return `Value::Bytes` for binary columns. PostgreSQL returns `bytea` as its text form (`\x…`), which is also valid input for an edit. SQLite also returns `Int`/`Float`. The MongoDB driver maps BSON as follows: null → `Null`, absent field → `Missing`, bool → `Bool`, Int32 → `Int`, Double → `Float`, String → `Text`, and every other type → `Json(canonical Extended JSON)`.
- **Browse.** `SELECT * FROM <ns>.<name> [WHERE (<filter>)] [ORDER BY …] LIMIT n OFFSET m`. When `sort` is `None` and there is a key, order by the key columns, so pages are stable. Documents always sort (by `_id` by default) because DocumentDB has no natural order.
- **Key and read-only.** `Page.key` is the primary key columns (`["_id"]` for documents). Set `read_only_reason` and leave `key` empty for views, for tables without a primary key, and for read-only connections.
- **Edits.** The UI sends raw text (`Edit::Text`) or `Edit::Null`. The driver builds typed statements:
  - PostgreSQL, MySQL, SQLite: use quoted string literals (`core::sql::*_literal`). The server converts them to the column type. Binary columns: input that starts with `0x` becomes a hex literal. SQLite: if the original value is `Int`/`Float` and the input is a number, write the number without quotes.
  - Documents: see section 6.
  - `preview()` returns the exact statements that `apply()` runs.
  - `apply()`: SQL runs in one transaction. Each UPDATE/DELETE must affect exactly 1 row, or everything rolls back with a clear error. `Insert` with no values uses `DEFAULT VALUES` (MySQL: `() VALUES ()`).
- **Execute.** Run in the given namespace. Return one `ResultSet` per statement. Cap rows at `max_rows` and set `truncated`. Statements without rows set `affected` and `message`. If a statement fails, return an error that names the statement number when it is known.
- **Session state.** The query editor of a connection uses one dedicated database session, so `SET`, `USE`, temp tables and `BEGIN … COMMIT` persist between runs. Browse and metadata use a different session, so a long query does not block the sidebar.
- **Cancel.** `cancel()` stops the running `execute` (PostgreSQL: cancel token; MySQL: `KILL QUERY <id>` from another session; SQLite: interrupt handle; MongoDB: no-op, the UI drops the future).
- **Read-only.** PostgreSQL: `SET default_transaction_read_only = on` on each session. MySQL: `SET SESSION TRANSACTION READ ONLY` on each session. SQLite: open the file read-only. MongoDB: refuse write methods and `apply()`.
- **Errors.** Give the server message, plus detail and hint where they exist. Do not show Rust debug output.
- **Timeouts.** Connect timeout 10 s. No query timeout (the user has Stop).

## 5. Per database

### PostgreSQL
- Config from fields or `url.parse::<tokio_postgres::Config>()`. Empty database means "postgres". `application_name = OurDB`.
- TLS: Off → `NoTls`. Prefer/Require → native-tls that accepts any certificate and host name (`SslMode::Prefer`/`Require`). Verify → native-tls with the CA file added, `SslMode::Require`. Through a tunnel, Verify still checks the chain but skips the host name.
- Namespaces = schemas except `pg_*` and `information_schema`. Default namespace = "public". `databases()` lists `pg_database` (no templates).
- Objects: `pg_class` relkind r/p/f → Table, v/m → View.
- Columns: `pg_attribute` + `format_type` + `pg_get_expr` + `pg_index.indisprimary`.
- Literals: read `standard_conforming_strings` at connect and pass it to `pg_literal`.
- native-tls on macOS supports TLS 1.2 at most. RDS and most servers accept it.

### MySQL
- `OptsBuilder` from fields or `Opts::from_url`. **Always** `prefer_socket(false)`: else mysql_async can silently switch to a local socket, also through a tunnel. Set `client_found_rows(true)`, so an UPDATE that does not change the value still counts as 1 row.
- TLS: Prefer tries TLS and falls back to plain TCP. Require skips checks. Verify adds the CA file. Through a tunnel, Verify skips the domain check.
- Namespaces = `SHOW DATABASES`. Default = the configured database.
- Objects and columns come from `information_schema` (`COLUMN_KEY = 'PRI'`).
- Text protocol: non-NULL values come as bytes. Binary charset (63) with a BLOB/BINARY/VARBINARY/BIT/GEOMETRY type → `Bytes`. Everything else → UTF-8 (lossy) `Text`.
- Literals: read `@@sql_mode`. NO_BACKSLASH_ESCAPES → `mysql_literal(s, false)`.

### SQLite
- Open `cfg.path` (no CREATE: a typo must not make a new file). Read-only → `SQLITE_OPEN_READ_ONLY`. `busy_timeout` 5 s. Two connections (browse, editor) behind mutexes, used through `spawn_blocking`.
- Namespaces = `PRAGMA database_list` names. Default = "main".
- Objects from `<ns>.sqlite_master` (no `sqlite_%`). Columns from `PRAGMA <ns>.table_info(<t>)`.
- Execute with `rusqlite::Batch`. Stop at the first error.

### MongoDB and DocumentDB
- Options from the URL (`ClientOptions::parse`) or from fields. Credential source "admin" unless the URL says other. `app_name = OurDB`. Connect and server selection timeouts 10 s.
- TLS: Off/Prefer → no TLS. Require → TLS, invalid certificates allowed. Verify → TLS with the CA file.
- **DocumentDB:** `retry_writes = false` always. Default TLS Verify with the AWS CA bundle: `https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem`.
- **Through a tunnel** (both kinds): `direct_connection = true`. With TLS on, allow invalid certificates, because the rustls backend cannot skip only the host name check. Tell the user in the connection dialog. A `mongodb+srv://` URL cannot go through a tunnel; give a clear error.
- Namespaces = `list_database_names` (if not authorized, fall back to the configured database). Objects = `list_collection_names` without `system.*`.
- Columns: sample 100 documents. Take the union of top-level fields in first-seen order (`_id` first), with the most common BSON type name.
- Browse: `find(filter).sort(sort or {_id: 1}).skip().limit()`. Grid columns = `_id`, then the union of top-level fields in this page in first-seen order. Absent → `Value::Missing`.
- Count: `count_documents(filter)`.
- Close cursors. Do not keep a cursor open per page (DocumentDB has a small cursor limit).

## 6. Documents: shell syntax and typed edits

**Shell JSON** (filter box, query editor, JSON documents): JSON5 (unquoted keys, single quotes, trailing commas) plus these helpers: `ObjectId("…")`, `ISODate("…")`, `new Date("…")`, `NumberLong(…)`, `NumberInt(…)`, `NumberDecimal("…")`. Convert helpers to Extended JSON, then call `Bson::try_from(serde_json::Value)`.

**Query editor subset.** One or more statements separated by `;` or new lines at the top level:
- `db.<coll>.<method>(…)` or `db.getCollection("<name>").<method>(…)`
- Read: `find(filter?, projection?)` with optional `.sort()`, `.skip()`, `.limit()`; `findOne`, `aggregate([...])`, `countDocuments(filter?)`, `estimatedDocumentCount()`, `distinct(field, filter?)`.
- Write: `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`, `deleteOne`, `deleteMany`.
- Other: `db.runCommand({...})`, `show dbs`, `show collections`.
- Results: documents are flattened like browse. A count or distinct gives one column. A write gives `affected` + `message` ("matched 3, modified 3").

**Typed edits** (`Change::Update`). The driver converts the input from the type of the original value:

| Original | Input accepted | Stored as |
|---|---|---|
| `Text` | any text | String |
| `Int` (Int32) | integer | Int32 if it fits, else Int64 |
| `Float` | number | Double |
| `Bool` | true/false | Boolean |
| `$numberLong` | integer | Int64 |
| `$numberDecimal` | decimal text | Decimal128 |
| `$oid` | 24 hex chars or `ObjectId("…")` | ObjectId |
| `$date` | ISO 8601 / RFC 3339, `ISODate("…")` or epoch ms | Date |
| document / array | shell JSON | the parsed BSON |
| `Null` / `Missing` | shell JSON value, else plain text | parsed value or String |

`Edit::Null` stores BSON null. `Change::InsertDoc` and `Change::ReplaceDoc` parse the whole document as shell JSON. `ReplaceDoc` keeps the `_id`. Preview text uses mongosh syntax, for example `db.users.updateOne({_id: ObjectId('…')}, {$set: {age: 31}})`.

## 7. User interface

- **Left panel:** "Connections" header with a `+` button. One row per connection: env color dot, name, type badge. Click → connect and expand. Right-click → Connect/Disconnect, New query, Edit, Duplicate, Delete. Under a connected connection: PostgreSQL database picker, filter box, namespaces (default one open), objects. Click an object → open its table tab.
- **Tabs:** table tabs and query tabs, with a close button and an env color strip. ⌘T new query, ⌘W close.
- **Table tab:** Data/Structure switch, filter box (Enter applies), Refresh, `+ Row` (SQL) or `+ Document` (JSON editor), Delete row, `Save (n)`, Discard, page controls, Count.
  - Grid: row number column, resizable columns, sort on header click (asc → desc → off). NULL is dim italic "NULL"; Missing is a dim "—". Numbers are right-aligned. Edited cells are yellow, new rows green, deleted rows red.
  - Double-click a cell to edit in place (Enter keeps, Esc cancels). Right-click a cell → Copy value, Copy row (TSV), Copy row (JSON), Set NULL, Filter by this value, Delete row.
  - Value panel on the right for the selected cell, with Apply and Set NULL. Documents also have "Edit document" (whole JSON → `ReplaceDoc`).
  - Save → a review dialog lists `preview()`, then Apply or Cancel. On success, reload. On error, keep the staged changes and show the error.
- **Query tab:** namespace picker, Run (⌘↵), Stop, History, Export CSV/JSON, editor with simple keyword colors, result tabs per statement, status line (rows, time, "showing first 10,000 rows").
- **Safety:** `core::sql::risky` → confirm dialog. On `Env::Prod`, `core::sql::is_write` → confirm dialog. Grid Save on Prod → the review dialog says "PRODUCTION".
- **Connection dialog:** fields as in section 2, defaults per type (`Kind::default_port`, `Kind::default_tls`). SQLite has a file picker. Test shows ✓ or the error.
- **Storage:** `connections.json` and `history.json` (last 100 queries per connection) in `~/Library/Application Support/OurDB/`. Passwords: Keychain, service "OurDB", account = connection id.
- **Look:** system light/dark theme, monospace font for the editor and the value panel.

## 8. Install

- `scripts/install.sh` builds a release binary, creates `OurDB.app`, signs it ad hoc and copies it to `/Applications`.
- `scripts/bundle-macos.sh` makes `dist/OurDB.app` and `dist/OurDB.dmg`.

## 9. Decisions made during the build

- **Stable pages:** a user sort also orders by the key columns (all drivers).
- **PostgreSQL:** the query editor sends one statement at a time, like psql, so each statement commits alone. A filter must be one expression (no `;`). `ssl*` options in a URL are ignored; the TLS field decides. A non-empty database field wins over the URL path (the database picker uses this). `COPY … STDIN/STDOUT` is refused.
- **MySQL:** the primary key comes from `information_schema.STATISTICS` (`INDEX_NAME = 'PRIMARY'`), because `COLUMN_KEY = 'PRI'` also marks a UNIQUE NOT NULL column in a table without a primary key. Pool: min 1, max 4 connections.
- **SQLite:** columns come from `pragma_table_xinfo` (generated columns show too). `cancel()` also stops a batch between statements.
- **MongoDB:** a URL that turns TLS on (`tls=true`, `mongodb+srv://`) keeps TLS even when the TLS field is Off. Only `authSource` changes the credential source. Read-only also refuses `aggregate` with `$out`/`$merge` and write commands in `runCommand`. DocumentDB with Verify and no CA file downloads the AWS bundle one time (`/usr/bin/curl`). JSON keys keep their order (serde_json `preserve_order`).
- **UI:** paging, sort, filter and Refresh are off while staged changes exist. Closing a tab with staged changes asks first. Disconnect closes the table tabs of that connection.
- **Contract:** `Driver::current_database()` (PostgreSQL) was added, so the picker shows the real database.

## 10. Tests

- Start test databases: `docker compose -f dev/compose.yml up -d` (PostgreSQL on 55432, MySQL on 53306, MongoDB on 57017; user `ourdb`, password `ourdb`; the MongoDB user is root in `admin`).
- Driver integration tests run when `OURDB_IT=1`: `OURDB_IT=1 cargo test -p ourdb-postgres`. Without it they return early.
- SQLite tests use a temp file and always run.
- The UI has no automated tests. Check it with a screenshot (`OURDB_SCREENSHOT=<file.png>`).
