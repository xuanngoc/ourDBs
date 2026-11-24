# OurDB

OurDB is a small, fast database client for macOS. It shows your data as a table, and you can edit it in place. It is one native Rust app (a 22 MB download) with no Electron and no Java.

Supported databases: **PostgreSQL, MySQL, SQLite, MongoDB, Amazon DocumentDB**.

The design rules are in [SPEC.md](SPEC.md).

## Install

1. Download **[OurDB.dmg](https://github.com/xuanngoc/ourDBs/releases/latest/download/OurDB.dmg)** (Apple Silicon and Intel, macOS 11 or later).
2. Open it and drag **OurDB** to **Applications**.
3. Open OurDB. The first time, macOS blocks it, because the app is not notarized by Apple:
   1. Click **Done** (do not move it to the Trash).
   2. Open **System Settings → Privacy & Security**.
   3. Next to "OurDB was blocked", click **Open Anyway**, then confirm.

   Or run one command in Terminal: `xattr -dr com.apple.quarantine /Applications/OurDB.app`

### Build from source

You need Rust one time.

```sh
brew install rustup && rustup default stable   # skip if you already have Rust
./scripts/install.sh                           # builds OurDB.app and copies it to /Applications
```

- `./scripts/bundle-macos.sh` makes `dist/OurDB.app`.
- `./scripts/bundle-macos.sh --universal --dmg` makes a universal app (Apple Silicon + Intel) and `dist/OurDB.dmg`. It needs `rustup target add x86_64-apple-darwin` one time.
- `cargo run -p ourdb` starts the app from the source folder.

### Publish a release

Push a version tag. GitHub Actions (`.github/workflows/release.yml`) builds the universal DMG and attaches it to a GitHub Release, so the download link above always gets the newest version.

```sh
git tag v0.1.0 && git push origin v0.1.0
```

## Use

1. Click **+** in the sidebar and fill in the connection form. Click **Test**, then **Save**.
2. Click the connection to open it. Then click a table or collection.
3. Edit cells (double-click), add rows (**+ Row** or **+ Document**) and delete rows. The changes are highlighted.
4. Click **Save (n)**. OurDB shows the exact SQL (or mongosh commands) before it runs them. SQL changes run in one transaction.
5. Press **⌘T** for a query tab. Press **⌘↵** to run the selection, or all text if nothing is selected.

| Key | Action |
|---|---|
| ⌘↵ | Run query |
| ⌘T / ⌘W | New query tab / close tab |
| ⌘S | Save staged changes |
| ⌘R | Reload |
| ⌘C | Copy the selected cell |
| Esc | Cancel a cell edit |

### Filters

- SQL: type the body of a WHERE clause, for example `age > 30 AND name LIKE 'A%'`.
- MongoDB/DocumentDB: type a filter in mongosh syntax, for example `{age: {$gt: 30}, _id: ObjectId("64b7f0c2a1b2c3d4e5f60718")}`.

### MongoDB query editor

OurDB runs a small mongosh subset, for example:

```js
db.users.find({age: {$gt: 30}}).sort({name: 1}).limit(20)
db.orders.aggregate([{$group: {_id: "$status", n: {$sum: 1}}}])
db.users.updateMany({active: false}, {$set: {archived: true}})
db.runCommand({dbStats: 1})
show collections
```

Supported methods: `find`, `findOne`, `aggregate`, `countDocuments`, `estimatedDocumentCount`, `distinct`, `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`, `deleteOne`, `deleteMany`, `runCommand`.

## Connections

- **Passwords** go to the macOS Keychain (service "OurDB"). The other settings go to `~/Library/Application Support/OurDB/connections.json`.
- **URL:** you can paste a full URL (`postgres://…`, `mysql://…`, `mongodb://…`, `mongodb+srv://…`) instead of the fields.
- **SSH tunnel:** type an SSH destination (`bastion`, `ec2-user@1.2.3.4` or `user@host:2222`) and, if needed, a key file (Advanced → SSH key). OurDB runs your system `ssh`, so `~/.ssh/config`, your keys and the ssh-agent work. Password login is not supported.
- **Read-only:** the server enforces it where it can (PostgreSQL and MySQL read-only sessions, SQLite read-only file). For MongoDB, OurDB refuses write commands.
- **Env tag:** Dev (green), Staging (yellow) or Prod (red). On Prod, OurDB asks before every write.
- **Safety:** OurDB always asks before DROP, TRUNCATE, ALTER … DROP, DELETE or UPDATE without WHERE, `deleteMany({})` and `drop()`.

### Amazon DocumentDB

1. Choose type **DocumentDB** and TLS **Verify**.
2. Leave the CA file empty: OurDB downloads the AWS bundle (`https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem`) one time to `~/Library/Application Support/OurDB/rds-global-bundle.pem`. You can also select your own copy.
3. DocumentDB is only reachable inside its VPC. From your Mac, set the SSH tunnel field to a bastion host.

OurDB always sets `retryWrites=false` for DocumentDB. Through an SSH tunnel, the certificate cannot match `localhost`, so OurDB turns off certificate checks for that connection (the SSH tunnel still encrypts the traffic to the bastion).

## Development

```sh
docker compose -f dev/compose.yml up -d          # PostgreSQL :55432, MySQL :53306, MongoDB :57017 (user/password ourdb)
./dev/seed.sh                                    # sample "shop" data in each database + dev/shop.sqlite
cargo test --workspace                           # unit tests + SQLite tests
OURDB_IT=1 cargo test --workspace                # also the PostgreSQL, MySQL and MongoDB tests
cargo run -p ourdb-ui --example demo             # the UI with an in-memory demo database
```

Project layout:

```
crates/core      Shared types, Driver trait, values, SQL quoting, export, SSH tunnel
crates/postgres  PostgreSQL driver (tokio-postgres)
crates/mysql     MySQL driver (mysql_async)
crates/sqlite    SQLite driver (rusqlite, SQLite compiled in)
crates/mongo     MongoDB + DocumentDB driver (mongodb 3)
crates/ui        egui/eframe user interface
crates/app       The `ourdb` binary
```

## Limits (v0.1)

- No schema editor, import, dump/restore or autocomplete.
- Edits need a primary key (`_id` for documents). Tables without one are read-only.
- A table tab exports only the page on screen. Use a query tab to export more rows (up to 10,000).
- PostgreSQL and MySQL use macOS native TLS: TLS 1.2 at most. With TLS **Prefer**, PostgreSQL does not fall back to plain TCP after a failed TLS handshake. MySQL **Verify** fails with certificates that MySQL makes itself, because macOS refuses certificates valid for more than 825 days (use **Require** or a real CA).
- MongoDB filters do not accept regex literals (`/abc/i`). Use `{$regex: "abc", $options: "i"}`.
- In a wide table, the vertical scrollbar is at the right end of the table. Scroll with the trackpad or mouse wheel.
- A macOS Keychain prompt pauses the window until you answer it.
- An `ATTACH` in the SQLite query editor does not show in the sidebar.
