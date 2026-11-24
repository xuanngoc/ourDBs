//! SQLite driver tests. They use a temp file, so they always run.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use ourdb_core::{
    BrowseRequest, Change, ConnConfig, Driver, Edit, Kind, ObjectKind, ObjectRef, Value,
};

const SCHEMA: &str = r#"
CREATE TABLE people (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL DEFAULT 'anon',
  note TEXT,
  score REAL,
  data BLOB,
  age INTEGER DEFAULT 7
);
INSERT INTO people VALUES
  (1, 'Alice', NULL, 1.5, X'00ff', 30),
  (2, 'Bob', '', 2.0, NULL, 40),
  (3, 'Carol', 'c', NULL, X'', 25),
  (4, 'Dave', 'd', 4.5, NULL, 35),
  (5, 'Eve', 'e', 5.0, NULL, 20);
CREATE TABLE tags (code TEXT PRIMARY KEY, label TEXT);
INSERT INTO tags VALUES ('c', 'third'), ('a', 'first'), ('b', 'second');
CREATE TABLE pairs (a TEXT, b INTEGER, v TEXT, PRIMARY KEY (b, a)) WITHOUT ROWID;
INSERT INTO pairs VALUES ('x', 2, 'x2'), ('y', 1, 'y1'), ('x', 1, 'x1');
CREATE TABLE nokey (x, y);
INSERT INTO nokey VALUES (1, 'one'), (2, 'two');
CREATE TABLE loose (id INTEGER PRIMARY KEY, v);
INSERT INTO loose VALUES (1, 5), (2, 'text');
CREATE TABLE logs (id INTEGER PRIMARY KEY AUTOINCREMENT, msg TEXT);
INSERT INTO logs (msg) VALUES ('hello');
CREATE VIEW adults AS SELECT id, name FROM people WHERE age >= 30;
"#;

/// A temp folder with a database file made from `SCHEMA`.
fn make_db() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    (dir, path.to_str().unwrap().to_string())
}

fn config(path: &str, read_only: bool) -> ConnConfig {
    ConnConfig {
        kind: Kind::Sqlite,
        path: path.into(),
        read_only,
        ..Default::default()
    }
}

async fn open(path: &str, read_only: bool) -> Arc<dyn Driver> {
    ourdb_sqlite::connect(&config(path, read_only))
        .await
        .unwrap()
}

fn obj(name: &str) -> ObjectRef {
    ObjectRef::new("main", name)
}

fn req(name: &str) -> BrowseRequest {
    BrowseRequest {
        object: obj(name),
        filter: String::new(),
        sort: None,
        limit: 100,
        offset: 0,
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn input(s: &str) -> Edit {
    Edit::Text(s.into())
}

/// All rows of a table in key order.
async fn rows(d: &Arc<dyn Driver>, name: &str) -> Vec<Vec<Value>> {
    d.browse(&req(name)).await.unwrap().result.rows
}

/// The values of one column of a result.
fn column(rows: &[Vec<Value>], i: usize) -> Vec<Value> {
    rows.iter().map(|r| r[i].clone()).collect()
}

/// Wait without the tokio `time` feature.
async fn pause(ms: u64) {
    tokio::task::spawn_blocking(move || std::thread::sleep(Duration::from_millis(ms)))
        .await
        .unwrap();
}

#[tokio::test]
async fn namespaces_objects_columns() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;
    assert_eq!(d.kind(), Kind::Sqlite);
    assert_eq!(d.namespaces().await.unwrap(), vec!["main"]);
    assert_eq!(d.default_namespace().as_deref(), Some("main"));

    // Sorted by name; sqlite_sequence is hidden.
    let objects: Vec<(String, ObjectKind)> = d
        .objects("main")
        .await
        .unwrap()
        .into_iter()
        .map(|o| (o.name, o.kind))
        .collect();
    let expected = [
        ("adults", ObjectKind::View),
        ("logs", ObjectKind::Table),
        ("loose", ObjectKind::Table),
        ("nokey", ObjectKind::Table),
        ("pairs", ObjectKind::Table),
        ("people", ObjectKind::Table),
        ("tags", ObjectKind::Table),
    ];
    assert_eq!(objects, expected.map(|(n, k)| (n.to_string(), k)));

    let cols = d.columns(&obj("people")).await.unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "note", "score", "data", "age"]);
    let types: Vec<&str> = cols.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(
        types,
        ["INTEGER", "TEXT", "TEXT", "REAL", "BLOB", "INTEGER"]
    );
    assert!(cols[0].key && !cols[1].key);
    assert!(!cols[1].nullable && cols[2].nullable);
    assert_eq!(cols[1].default.as_deref(), Some("'anon'"));
    assert_eq!(cols[5].default.as_deref(), Some("7"));
    assert_eq!(cols[2].default, None);

    // WITHOUT ROWID table with a composite key in (b, a) order.
    let cols = d.columns(&obj("pairs")).await.unwrap();
    let keys: Vec<(&str, bool)> = cols.iter().map(|c| (c.name.as_str(), c.key)).collect();
    assert_eq!(keys, [("a", true), ("b", true), ("v", false)]);
    let page = d.browse(&req("pairs")).await.unwrap();
    assert_eq!(page.key, ["b", "a"]);
    assert_eq!(page.read_only_reason, None);
    assert_eq!(
        column(&page.result.rows, 2),
        [text("x1"), text("y1"), text("x2")]
    );

    // A view has columns but no key.
    let cols = d.columns(&obj("adults")).await.unwrap();
    assert_eq!(cols.len(), 2);
    assert!(cols.iter().all(|c| !c.key));

    let err = d.columns(&obj("nope")).await.unwrap_err().to_string();
    assert!(err.contains("no such table"), "{err}");
}

#[tokio::test]
async fn view_and_table_without_key_are_read_only() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;

    let page = d.browse(&req("adults")).await.unwrap();
    assert!(page.key.is_empty());
    assert_eq!(page.read_only_reason.as_deref(), Some("view"));
    assert_eq!(page.result.columns, ["id", "name"]);
    assert_eq!(page.result.rows.len(), 3);

    let page = d.browse(&req("nokey")).await.unwrap();
    assert!(page.key.is_empty());
    assert_eq!(page.read_only_reason.as_deref(), Some("no primary key"));
    assert_eq!(page.result.rows.len(), 2);

    // An edit without a key is refused before anything runs.
    let change = Change::Delete { key: vec![] };
    let err = d
        .apply(&obj("nokey"), &[change])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no primary key"), "{err}");
    assert_eq!(rows(&d, "nokey").await.len(), 2);
}

#[tokio::test]
async fn browse_filter_sort_paging_and_key_order() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;

    let page = d.browse(&req("people")).await.unwrap();
    assert_eq!(page.key, ["id"]);
    assert_eq!(
        page.result.columns,
        ["id", "name", "note", "score", "data", "age"]
    );
    assert_eq!(
        page.result.types,
        ["INTEGER", "TEXT", "TEXT", "REAL", "BLOB", "INTEGER"]
    );
    assert_eq!(page.result.rows.len(), 5);
    assert_eq!(page.result.rows[0][0], Value::Int(1));
    assert_eq!(page.result.rows[0][3], Value::Float(1.5));

    // The table order is c, a, b; with no sort the key order is used.
    assert_eq!(
        column(&rows(&d, "tags").await, 0),
        [text("a"), text("b"), text("c")]
    );

    let mut r = req("people");
    r.filter = "age >= 30 -- a comment must not break the query".into();
    let page = d.browse(&r).await.unwrap();
    assert_eq!(
        column(&page.result.rows, 1),
        [text("Alice"), text("Bob"), text("Dave")]
    );

    r.sort = Some(("name".into(), false));
    let page = d.browse(&r).await.unwrap();
    assert_eq!(
        column(&page.result.rows, 1),
        [text("Dave"), text("Bob"), text("Alice")]
    );

    let mut r = req("people");
    r.sort = Some(("age".into(), true));
    r.limit = 2;
    r.offset = 1;
    let page = d.browse(&r).await.unwrap();
    assert_eq!(column(&page.result.rows, 1), [text("Carol"), text("Alice")]);

    let mut r = req("people");
    r.filter = "no_such_column = 1".into();
    let err = d.browse(&r).await.unwrap_err().to_string();
    assert!(err.contains("no such column"), "{err}");

    // Only one statement is allowed in a filter.
    r.filter = "1); DROP TABLE people; --".into();
    assert!(d.browse(&r).await.is_err());
    assert_eq!(rows(&d, "people").await.len(), 5);
}

#[tokio::test]
async fn null_empty_text_and_blobs() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;
    let rows = rows(&d, "people").await;
    assert_eq!(rows[0][2], Value::Null);
    assert_eq!(rows[1][2], text(""));
    assert_eq!(rows[0][4], Value::Bytes(vec![0x00, 0xff]));
    assert_eq!(rows[2][4], Value::Bytes(vec![]));
    assert_eq!(rows[1][4], Value::Null);
    assert_eq!(rows[2][3], Value::Null);
}

#[tokio::test]
async fn count_rows() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;
    assert_eq!(d.count(&req("people")).await.unwrap(), 5);
    let mut r = req("people");
    r.filter = "note IS NULL OR note = ''".into();
    r.limit = 1;
    assert_eq!(d.count(&r).await.unwrap(), 2);
}

#[tokio::test]
async fn apply_update_insert_delete_with_tricky_text() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;
    let tricky = "O'Brien \\ back\\slash\nnew line 🦀";
    let changes = [
        Change::Update {
            key: vec![("id".into(), Value::Int(1))],
            set: vec![
                ("name".into(), text("Alice"), input("O'Brien")),
                ("note".into(), Value::Null, input(tricky)),
                ("score".into(), Value::Float(1.5), input("2.25")),
                ("data".into(), Value::Bytes(vec![0, 255]), input("0xCAFE")),
                ("age".into(), Value::Int(30), Edit::Null),
            ],
        },
        Change::Insert { values: vec![] },
        Change::Insert {
            values: vec![
                ("name".into(), input("🦀 Ferris")),
                ("note".into(), input("a'b\\c")),
            ],
        },
        Change::Delete {
            key: vec![("id".into(), Value::Int(2))],
        },
    ];
    assert_eq!(d.apply(&obj("people"), &changes).await.unwrap(), 4);

    let people = rows(&d, "people").await;
    let ids: Vec<Value> = column(&people, 0);
    assert_eq!(ids, [1, 3, 4, 5, 6, 7].map(Value::Int));
    assert_eq!(
        people[0],
        [
            Value::Int(1),
            text("O'Brien"),
            text(tricky),
            Value::Float(2.25),
            Value::Bytes(vec![0xca, 0xfe]),
            Value::Null
        ]
    );
    // DEFAULT VALUES uses the column defaults.
    assert_eq!(
        people[4],
        [
            Value::Int(6),
            text("anon"),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Int(7)
        ]
    );
    // Columns that are not listed get their defaults.
    assert_eq!(
        people[5],
        [
            Value::Int(7),
            text("🦀 Ferris"),
            text("a'b\\c"),
            Value::Null,
            Value::Null,
            Value::Int(7)
        ]
    );

    // A composite key in a WITHOUT ROWID table.
    let change = Change::Update {
        key: vec![("b".into(), Value::Int(1)), ("a".into(), text("y"))],
        set: vec![("v".into(), text("y1"), input("new"))],
    };
    d.apply(&obj("pairs"), &[change]).await.unwrap();
    assert_eq!(
        column(&rows(&d, "pairs").await, 2),
        [text("x1"), text("new"), text("x2")]
    );
}

#[tokio::test]
async fn numbers_stay_numbers_in_columns_without_type() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;
    let changes = [
        Change::Update {
            key: vec![("id".into(), Value::Int(1))],
            set: vec![("v".into(), Value::Int(5), input("6"))],
        },
        // The original is text, so the input stays text.
        Change::Update {
            key: vec![("id".into(), Value::Int(2))],
            set: vec![("v".into(), text("text"), input("7"))],
        },
    ];
    let preview = d.preview(&obj("loose"), &changes).unwrap();
    assert_eq!(
        preview[0],
        r#"UPDATE "main"."loose" SET "v" = 6 WHERE "id" = 1"#
    );
    assert_eq!(
        preview[1],
        r#"UPDATE "main"."loose" SET "v" = '7' WHERE "id" = 2"#
    );
    d.apply(&obj("loose"), &changes).await.unwrap();
    assert_eq!(
        column(&rows(&d, "loose").await, 1),
        [Value::Int(6), text("7")]
    );
}

#[tokio::test]
async fn update_and_delete_must_hit_exactly_one_row() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;
    let good = Change::Update {
        key: vec![("id".into(), Value::Int(1))],
        set: vec![("name".into(), text("Alice"), input("Changed"))],
    };
    let missing = Change::Update {
        key: vec![("id".into(), Value::Int(999))],
        set: vec![("name".into(), text("x"), input("y"))],
    };
    let err = d
        .apply(&obj("people"), &[good.clone(), missing])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("statement 2 changed 0 rows, expected exactly 1"),
        "{err}"
    );
    // The first update rolled back too.
    assert_eq!(rows(&d, "people").await[0][1], text("Alice"));

    let delete = Change::Delete {
        key: vec![("id".into(), Value::Int(999))],
    };
    let err = d
        .apply(&obj("people"), &[delete])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("changed 0 rows"), "{err}");

    // A server error also rolls back everything.
    let bad = Change::Update {
        key: vec![("id".into(), Value::Int(2))],
        set: vec![("name".into(), text("Bob"), Edit::Null)],
    };
    let err = d
        .apply(&obj("people"), &[good, bad])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("statement 2") && err.contains("NOT NULL"),
        "{err}"
    );
    assert_eq!(rows(&d, "people").await[0][1], text("Alice"));
}

#[tokio::test]
async fn preview_is_what_apply_runs() {
    let changes = [
        Change::Update {
            key: vec![("id".into(), Value::Int(1))],
            set: vec![
                ("name".into(), text("Alice"), input("O'Brien")),
                ("score".into(), Value::Float(1.5), input("2.25")),
                ("data".into(), Value::Bytes(vec![0]), input("0x00ff")),
            ],
        },
        Change::Insert { values: vec![] },
        Change::Insert {
            values: vec![
                ("name".into(), input("🦀 a\nb;c")),
                ("note".into(), Edit::Null),
            ],
        },
        Change::Delete {
            key: vec![("id".into(), Value::Int(2))],
        },
    ];
    let (_dir_a, path_a) = make_db();
    let (_dir_b, path_b) = make_db();
    let a = open(&path_a, false).await;
    let b = open(&path_b, false).await;

    let preview = a.preview(&obj("people"), &changes).unwrap();
    assert_eq!(
        preview,
        [
            r#"UPDATE "main"."people" SET "name" = 'O''Brien', "score" = 2.25, "data" = X'00ff' WHERE "id" = 1"#,
            r#"INSERT INTO "main"."people" DEFAULT VALUES"#,
            "INSERT INTO \"main\".\"people\" (\"name\", \"note\") VALUES ('🦀 a\nb;c', NULL)",
            r#"DELETE FROM "main"."people" WHERE "id" = 2"#,
        ]
    );

    // Run the preview text by hand on B and apply the changes on A.
    a.apply(&obj("people"), &changes).await.unwrap();
    b.execute(None, &preview.join(";\n"), 100).await.unwrap();
    assert_eq!(rows(&a, "people").await, rows(&b, "people").await);

    let err = a
        .preview(&obj("people"), &[Change::InsertDoc { json: "{}".into() }])
        .unwrap_err();
    assert!(err.to_string().contains("not supported"));
}

#[tokio::test]
async fn read_only_open_blocks_writes() {
    let (_dir, path) = make_db();
    let d = open(&path, true).await;

    let page = d.browse(&req("people")).await.unwrap();
    assert!(page.key.is_empty());
    assert_eq!(
        page.read_only_reason.as_deref(),
        Some("read-only connection")
    );
    assert_eq!(page.result.rows.len(), 5);

    let err = d
        .execute(None, "INSERT INTO people (name) VALUES ('x')", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("readonly"), "{err}");
    let change = Change::Delete {
        key: vec![("id".into(), Value::Int(1))],
    };
    let err = d
        .apply(&obj("people"), &[change])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("readonly"), "{err}");

    // Reads still work, and nothing changed.
    let out = d
        .execute(None, "SELECT count(*) FROM people", 10)
        .await
        .unwrap();
    assert_eq!(out[0].rows, [[Value::Int(5)]]);
}

#[tokio::test]
async fn missing_file_is_an_error_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("typo.db");
    let path = path.to_str().unwrap();
    let err = ourdb_sqlite::connect(&config(path, false))
        .await
        .err()
        .unwrap()
        .to_string();
    assert_eq!(err, format!("file not found: {path}"));
    assert!(!std::path::Path::new(path).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_a_long_query() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;
    let runner = d.clone();
    let task = tokio::spawn(async move {
        let endless = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c";
        runner.execute(None, endless, 10).await
    });
    pause(300).await;
    assert!(!task.is_finished(), "the query ended too early");

    // The sidebar session is free while the editor session is busy.
    assert_eq!(d.objects("main").await.unwrap().len(), 7);

    let start = Instant::now();
    d.cancel().await;
    while !task.is_finished() {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "cancel did not stop the query"
        );
        pause(10).await;
    }
    let err = task.await.unwrap().unwrap_err().to_string();
    assert!(
        err.contains("statement 1") && err.contains("interrupted"),
        "{err}"
    );

    // The editor session works again after a cancel.
    let out = d.execute(None, "SELECT 1", 10).await.unwrap();
    assert_eq!(out[0].rows, [[Value::Int(1)]]);
}

#[tokio::test]
async fn execute_many_statements() {
    let (_dir, path) = make_db();
    let d = open(&path, false).await;

    let text = "CREATE TEMP TABLE t (x INTEGER);\n\
                INSERT INTO t VALUES (1), (2);\n\
                -- only a comment;\n\
                SELECT x, x * 2 AS twice FROM t ORDER BY x;\n\
                UPDATE people SET age = age + 1 WHERE age > 30";
    let out = d.execute(Some("main"), text, 100).await.unwrap();
    assert_eq!(out.len(), 4);
    assert_eq!(out[0].affected, Some(0));
    assert_eq!(out[1].affected, Some(2));
    assert_eq!(out[1].message.as_deref(), Some("2 rows affected"));
    assert_eq!(out[2].columns, ["x", "twice"]);
    assert_eq!(out[2].types, ["INTEGER", ""]);
    assert_eq!(
        out[2].rows,
        [
            [Value::Int(1), Value::Int(2)],
            [Value::Int(2), Value::Int(4)]
        ]
    );
    assert_eq!(out[2].affected, None);
    assert_eq!(out[3].affected, Some(2));

    // The temp table stays in the editor session.
    let out = d
        .execute(None, "SELECT count(*) FROM t", 100)
        .await
        .unwrap();
    assert_eq!(out[0].rows, [[Value::Int(2)]]);

    // Row cap and `truncated`.
    let fifty =
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 50) SELECT x FROM c";
    let out = d.execute(None, fifty, 10).await.unwrap();
    assert_eq!(out[0].rows.len(), 10);
    assert!(out[0].truncated);
    let out = d.execute(None, fifty, 50).await.unwrap();
    assert_eq!(out[0].rows.len(), 50);
    assert!(!out[0].truncated);

    // A runtime error in statement 2 stops the run.
    let err = d
        .execute(None, "SELECT 1; SELECT * FROM nope; SELECT 2", 10)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(err, "statement 2: no such table: nope");

    // A syntax error in statement 2.
    let err = d
        .execute(None, "SELECT 1;\nSELEC 2", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("statement 2: ") && err.contains("syntax error"),
        "{err}"
    );

    // Attached databases are reachable as ns.table.
    let (_dir2, other) = make_db();
    let attach = format!(
        "ATTACH {} AS other; SELECT count(*) FROM other.tags",
        ourdb_core::sql::sqlite_literal(&other)
    );
    let out = d.execute(None, &attach, 10).await.unwrap();
    assert_eq!(out[1].rows, [[Value::Int(3)]]);
}
