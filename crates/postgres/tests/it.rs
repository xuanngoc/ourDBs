//! Integration tests against the dev PostgreSQL (dev/compose.yml, port 55432).
//! They run only with OURDB_IT=1. Each test takes a lock and builds the
//! `it_pg` schema again, so tests do not see each other's changes.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use ourdb_core::{
    BrowseRequest, Change, ConnConfig, Driver, Edit, Kind, ObjectKind, ObjectRef, Page, TlsMode,
    Value,
};
use ourdb_postgres::connect;

static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const SETUP: &str = r#"
DROP SCHEMA IF EXISTS it_pg CASCADE;
CREATE SCHEMA it_pg;
CREATE TABLE it_pg.things (
    id serial PRIMARY KEY,
    i int,
    big bigint,
    num numeric(10,2),
    flag boolean,
    name text,
    code varchar(20),
    j json,
    jb jsonb,
    ts timestamp,
    tstz timestamptz,
    d date,
    u uuid,
    b bytea,
    arr int[],
    note text,
    status text NOT NULL DEFAULT 'new'
);
INSERT INTO it_pg.things (i, big, num, flag, name, code, j, jb, ts, tstz, d, u, b, arr, note) VALUES
    (1, 9000000000, 12.5, true, 'alpha', 'A1', '{"a": 1}', '{"b":[1,2]}', '2024-01-02 03:04:05',
     '2024-01-02 03:04:05+00', '2024-01-02', '00000000-0000-0000-0000-000000000001', '\xdeadbeef', '{1,2,3}', NULL),
    (2, NULL, NULL, false, 'beta', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ''),
    (3, NULL, NULL, NULL, 'gamma', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 'x'),
    (4, NULL, NULL, NULL, 'delta', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
    (5, NULL, NULL, NULL, 'epsilon', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
CREATE VIEW it_pg.things_view AS SELECT id, name FROM it_pg.things;
CREATE TABLE it_pg.nokey (a int, b text);
INSERT INTO it_pg.nokey VALUES (1, 'x'), (1, 'y');
CREATE TABLE it_pg.pair (k2 int, k1 text, v text, PRIMARY KEY (k1, k2));
INSERT INTO it_pg.pair VALUES (2, 'a', 'a2'), (1, 'b', 'b1'), (1, 'a', 'a1');
CREATE TABLE it_pg.defaults (id serial PRIMARY KEY, created timestamptz NOT NULL DEFAULT now(), label text DEFAULT 'none');
CREATE VIEW it_pg.whoami AS SELECT pg_backend_pid() AS pid;
"#;

fn enabled() -> bool {
    std::env::var("OURDB_IT").as_deref() == Ok("1")
}

fn cfg() -> ConnConfig {
    ConnConfig {
        kind: Kind::Postgres,
        host: "127.0.0.1".into(),
        port: 55432,
        user: "ourdb".into(),
        password: "ourdb".into(),
        database: "ourdb".into(),
        tls: TlsMode::Off,
        ..Default::default()
    }
}

async fn setup() -> Arc<dyn Driver> {
    let d = connect(&cfg()).await.expect("connect");
    d.execute(None, SETUP, 100).await.expect("setup");
    d
}

fn obj(name: &str) -> ObjectRef {
    ObjectRef::new("it_pg", name)
}

fn req(
    name: &str,
    filter: &str,
    sort: Option<(&str, bool)>,
    limit: u32,
    offset: u64,
) -> BrowseRequest {
    BrowseRequest {
        object: obj(name),
        filter: filter.into(),
        sort: sort.map(|(c, asc)| (c.to_string(), asc)),
        limit,
        offset,
    }
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

fn col(page: &Page, name: &str) -> usize {
    page.result
        .columns
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

/// Values of one column of a page.
fn column(page: &Page, name: &str) -> Vec<Value> {
    let i = col(page, name);
    page.result.rows.iter().map(|r| r[i].clone()).collect()
}

fn texts(items: &[&str]) -> Vec<Value> {
    items.iter().map(|s| t(s)).collect()
}

#[tokio::test]
async fn catalog() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;

    let ns = d.namespaces().await.unwrap();
    assert!(
        ns.contains(&"it_pg".to_string()) && ns.contains(&"public".to_string()),
        "{ns:?}"
    );
    assert!(
        !ns.iter()
            .any(|n| n.starts_with("pg_") || n == "information_schema"),
        "{ns:?}"
    );
    assert_eq!(d.default_namespace().as_deref(), Some("public"));

    let objs: Vec<(String, ObjectKind)> = d
        .objects("it_pg")
        .await
        .unwrap()
        .into_iter()
        .map(|o| (o.name, o.kind))
        .collect();
    let want = [
        ("defaults", ObjectKind::Table),
        ("nokey", ObjectKind::Table),
        ("pair", ObjectKind::Table),
        ("things", ObjectKind::Table),
        ("things_view", ObjectKind::View),
        ("whoami", ObjectKind::View),
    ];
    assert_eq!(objs, want.map(|(n, k)| (n.to_string(), k)));

    let cols = d.columns(&obj("things")).await.unwrap();
    let types: Vec<&str> = cols.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(
        types,
        [
            "integer",
            "integer",
            "bigint",
            "numeric(10,2)",
            "boolean",
            "text",
            "character varying(20)",
            "json",
            "jsonb",
            "timestamp without time zone",
            "timestamp with time zone",
            "date",
            "uuid",
            "bytea",
            "integer[]",
            "text",
            "text"
        ]
    );
    let keys: Vec<&str> = cols
        .iter()
        .filter(|c| c.key)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(keys, ["id"]);
    assert!(!cols[0].nullable);
    assert!(
        cols[0].default.as_deref().unwrap().starts_with("nextval("),
        "{:?}",
        cols[0].default
    );
    let status = cols.iter().find(|c| c.name == "status").unwrap();
    assert!(!status.nullable);
    assert_eq!(status.default.as_deref(), Some("'new'::text"));
    assert!(cols[1].nullable && cols[1].default.is_none());

    let pair = d.columns(&obj("pair")).await.unwrap();
    let flags: Vec<(&str, bool)> = pair.iter().map(|c| (c.name.as_str(), c.key)).collect();
    assert_eq!(flags, [("k2", true), ("k1", true), ("v", false)]);

    let view = d.columns(&obj("things_view")).await.unwrap();
    assert_eq!(view.len(), 2);
    assert!(view.iter().all(|c| !c.key));

    let dbs = d.databases().await.unwrap();
    assert!(dbs.contains(&"ourdb".to_string()), "{dbs:?}");
    assert!(!dbs.iter().any(|n| n.starts_with("template")), "{dbs:?}");
}

#[tokio::test]
async fn browse_and_count() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;

    let p = d.browse(&req("things", "", None, 50, 0)).await.unwrap();
    assert_eq!(p.key, ["id"]);
    assert_eq!(p.read_only_reason, None);
    assert_eq!(p.result.columns.len(), 17);
    assert_eq!(p.result.types.len(), 17);
    assert_eq!(p.result.types[col(&p, "num")], "numeric(10,2)");
    assert_eq!(p.result.types[col(&p, "arr")], "integer[]");
    // Default order: the primary key.
    assert_eq!(column(&p, "id"), texts(&["1", "2", "3", "4", "5"]));

    // Values come as psql shows them.
    let row = &p.result.rows[0];
    let get = |name: &str| row[col(&p, name)].clone();
    assert_eq!(get("big"), t("9000000000"));
    assert_eq!(get("num"), t("12.50"));
    assert_eq!(get("flag"), t("t"));
    assert_eq!(get("code"), t("A1"));
    assert_eq!(get("j"), t(r#"{"a": 1}"#));
    assert_eq!(get("jb"), t(r#"{"b": [1, 2]}"#));
    assert_eq!(get("ts"), t("2024-01-02 03:04:05"));
    assert!(matches!(get("tstz"), Value::Text(s) if s.starts_with("2024-01-02 ")));
    assert_eq!(get("d"), t("2024-01-02"));
    assert_eq!(get("u"), t("00000000-0000-0000-0000-000000000001"));
    assert_eq!(get("b"), t(r"\xdeadbeef"));
    assert_eq!(get("arr"), t("{1,2,3}"));

    // NULL and the empty string are different.
    assert_eq!(
        column(&p, "note"),
        vec![Value::Null, t(""), t("x"), Value::Null, Value::Null]
    );

    // Filter, sort and paging.
    let p = d
        .browse(&req("things", "name LIKE '%ta'", None, 50, 0))
        .await
        .unwrap();
    assert_eq!(column(&p, "id"), texts(&["2", "4"]));
    let p = d
        .browse(&req("things", "", Some(("name", false)), 50, 0))
        .await
        .unwrap();
    assert_eq!(
        column(&p, "name"),
        texts(&["gamma", "epsilon", "delta", "beta", "alpha"])
    );
    let p = d.browse(&req("things", "", None, 2, 2)).await.unwrap();
    assert_eq!(column(&p, "id"), texts(&["3", "4"]));
    let p = d
        .browse(&req(
            "things",
            "id > 1 -- a comment",
            Some(("id", false)),
            2,
            1,
        ))
        .await
        .unwrap();
    assert_eq!(column(&p, "id"), texts(&["4", "3"]));

    // No rows: the columns are still there.
    let p = d
        .browse(&req("things", "false", None, 50, 0))
        .await
        .unwrap();
    assert_eq!(p.result.columns.len(), 17);
    assert!(p.result.rows.is_empty());

    // Count.
    assert_eq!(d.count(&req("things", "", None, 1, 0)).await.unwrap(), 5);
    assert_eq!(
        d.count(&req("things", "id > 2", None, 1, 0)).await.unwrap(),
        3
    );

    // Bad filters give clear errors.
    let e = d
        .browse(&req("things", "nme = 1", None, 50, 0))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains(r#"column "nme" does not exist"#), "{e}");
    assert!(e.ends_with("At column 1 of the filter."), "{e}");
    let e = d
        .count(&req("things", "id > 1 and\n  nme = 1", None, 1, 0))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.ends_with("At line 2, column 3 of the filter."), "{e}");
    let e = d
        .browse(&req(
            "things",
            "true); delete from it_pg.things; select (1",
            None,
            50,
            0,
        ))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("one expression"), "{e}");
    assert_eq!(d.count(&req("things", "", None, 1, 0)).await.unwrap(), 5);
}

#[tokio::test]
async fn read_only_reasons_and_composite_key() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;

    let p = d
        .browse(&req("things_view", "", None, 50, 0))
        .await
        .unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("view"));
    assert_eq!(p.result.types, ["integer", "text"]);

    let p = d.browse(&req("nokey", "", None, 50, 0)).await.unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("no primary key"));

    // The key is in primary key order (k1, k2), not in column order.
    let p = d.browse(&req("pair", "", None, 50, 0)).await.unwrap();
    assert_eq!(p.key, ["k1", "k2"]);
    assert_eq!(column(&p, "v"), texts(&["a1", "a2", "b1"]));
}

#[tokio::test]
async fn execute_statements() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;

    let r = d
        .execute(
            Some("it_pg"),
            "select id, name from things where id = 1;\n\
             update things set name = 'changed' where id <= 2;\n\
             select name from things where id = 1; -- done",
            100,
        )
        .await
        .unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].columns, ["id", "name"]);
    assert_eq!(r[0].types, ["", ""]);
    assert_eq!(r[0].rows, vec![texts(&["1", "alpha"])]);
    assert_eq!(r[0].affected, None);
    assert!(r[1].columns.is_empty());
    assert_eq!(r[1].affected, Some(2));
    assert_eq!(r[1].message.as_deref(), Some("UPDATE 2"));
    assert_eq!(r[2].rows, vec![texts(&["changed"])]);

    // The error names the statement and the line.
    let e = d
        .execute(Some("it_pg"), "select 1;\nselect * from nope", 100)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.starts_with("Statement 2: "), "{e}");
    assert!(e.contains(r#"relation "nope" does not exist"#), "{e}");
    assert!(e.contains("At line 2, column 15."), "{e}");

    // DETAIL and HINT lines.
    let e = d
        .execute(None, "insert into it_pg.things (id) values (1)", 100)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("duplicate key value") && e.contains("DETAIL: Key (id)=(1) already exists."),
        "{e}"
    );
    let e = d
        .execute(None, "select nam from it_pg.things", 100)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("HINT: "), "{e}");

    // Row cap.
    let r = d
        .execute(None, "select generate_series(1, 10) as n", 3)
        .await
        .unwrap();
    assert_eq!(r[0].rows.len(), 3);
    assert!(r[0].truncated);

    // No rows: the columns are still there. NULL is not ''.
    let r = d
        .execute(
            None,
            "select id, name from it_pg.things where false; select null::text, ''",
            10,
        )
        .await
        .unwrap();
    assert_eq!(r[0].columns, ["id", "name"]);
    assert!(r[0].rows.is_empty());
    assert_eq!(r[1].rows, vec![vec![Value::Null, t("")]]);

    // Statements without rows.
    let r = d
        .execute(
            Some("it_pg"),
            "create table tmp_ddl (x int); insert into tmp_ddl values (1), (2)",
            10,
        )
        .await;
    let r = r.unwrap();
    assert_eq!(r[0].message.as_deref(), Some("CREATE"));
    assert_eq!(r[1].affected, Some(2));
    assert_eq!(r[1].message.as_deref(), Some("INSERT 2"));

    // Session state persists in the editor session.
    d.execute(None, "create temp table tmp_x as select 42 as v", 10)
        .await
        .unwrap();
    let r = d.execute(None, "select v from tmp_x", 10).await.unwrap();
    assert_eq!(r[0].rows, vec![texts(&["42"])]);

    // A failed transaction does not lock the user out: ROLLBACK still runs.
    d.execute(Some("it_pg"), "begin", 10).await.unwrap();
    assert!(d.execute(Some("it_pg"), "select 1/0", 10).await.is_err());
    d.execute(Some("it_pg"), "rollback", 10).await.unwrap();
    let r = d
        .execute(Some("it_pg"), "select count(*) from things", 10)
        .await
        .unwrap();
    assert_eq!(r[0].rows, vec![texts(&["5"])]);

    // Empty text and comments only.
    assert!(
        d.execute(None, "  -- nothing\n", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

fn edits() -> Vec<Change> {
    vec![
        Change::Update {
            key: vec![("id".into(), t("1"))],
            set: vec![
                (
                    "name".into(),
                    t("alpha"),
                    Edit::Text("O'Brien \\ back\\slash\nnew line 😀".into()),
                ),
                ("note".into(), Value::Null, Edit::Text(String::new())),
                ("code".into(), t("A1"), Edit::Null),
                ("b".into(), t(r"\xdeadbeef"), Edit::Text("0xCAFE".into())),
            ],
        },
        Change::Insert {
            values: vec![
                ("name".into(), Edit::Text("new row".into())),
                ("i".into(), Edit::Text("77".into())),
            ],
        },
        Change::Delete {
            key: vec![("id".into(), t("5"))],
        },
    ]
}

/// All rows of a table, ordered by the key.
async fn all_rows(d: &Arc<dyn Driver>, name: &str) -> Vec<Vec<Value>> {
    d.browse(&req(name, "", None, 1000, 0))
        .await
        .unwrap()
        .result
        .rows
}

#[tokio::test]
async fn apply_changes() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;
    let o = obj("things");
    d.browse(&req("things", "", None, 50, 0)).await.unwrap();

    assert_eq!(d.apply(&o, &edits()).await.unwrap(), 3);

    let p = d
        .browse(&req("things", "id = 1", None, 50, 0))
        .await
        .unwrap();
    let row = &p.result.rows[0];
    assert_eq!(
        row[col(&p, "name")],
        t("O'Brien \\ back\\slash\nnew line 😀")
    );
    assert_eq!(row[col(&p, "note")], t(""));
    assert_eq!(row[col(&p, "code")], Value::Null);
    assert_eq!(row[col(&p, "b")], t(r"\xcafe"));

    // The insert got the defaults for the columns it did not list.
    let p = d
        .browse(&req("things", "name = 'new row'", None, 50, 0))
        .await
        .unwrap();
    assert_eq!(p.result.rows.len(), 1);
    assert_eq!(column(&p, "id"), texts(&["6"]));
    assert_eq!(column(&p, "i"), texts(&["77"]));
    assert_eq!(column(&p, "status"), texts(&["new"]));

    assert_eq!(
        d.count(&req("things", "id = 5", None, 1, 0)).await.unwrap(),
        0
    );

    // An insert without values uses DEFAULT VALUES.
    let o2 = obj("defaults");
    let ins = [Change::Insert { values: vec![] }];
    assert_eq!(
        d.preview(&o2, &ins).unwrap(),
        [r#"INSERT INTO "it_pg"."defaults" DEFAULT VALUES"#]
    );
    d.apply(&o2, &ins).await.unwrap();
    let p = d.browse(&req("defaults", "", None, 50, 0)).await.unwrap();
    assert_eq!(column(&p, "label"), texts(&["none"]));

    // Composite key.
    let up = [Change::Update {
        key: vec![("k1".into(), t("a")), ("k2".into(), t("2"))],
        set: vec![("v".into(), t("a2"), Edit::Text("A2!".into()))],
    }];
    d.apply(&obj("pair"), &up).await.unwrap();
    let p = d.browse(&req("pair", "", None, 50, 0)).await.unwrap();
    assert_eq!(column(&p, "v"), texts(&["a1", "A2!", "b1"]));
}

#[tokio::test]
async fn apply_exactly_one_row_and_rollback() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;
    let o = obj("things");
    let before = all_rows(&d, "things").await;

    // The second update hits no row: nothing is saved.
    let changes = [
        Change::Update {
            key: vec![("id".into(), t("1"))],
            set: vec![("name".into(), t("alpha"), Edit::Text("first".into()))],
        },
        Change::Update {
            key: vec![("id".into(), t("999"))],
            set: vec![("name".into(), Value::Null, Edit::Text("ghost".into()))],
        },
    ];
    let e = d.apply(&o, &changes).await.unwrap_err().to_string();
    assert!(
        e.contains("Change 2 of 2 failed")
            && e.contains("changed 0 rows")
            && e.contains("exactly 1 row"),
        "{e}"
    );
    assert!(e.contains(r#"WHERE "id" = '999'"#), "{e}");
    assert_eq!(all_rows(&d, "things").await, before);

    // A server error in the second change: nothing is saved.
    let changes = [
        Change::Delete {
            key: vec![("id".into(), t("2"))],
        },
        Change::Update {
            key: vec![("id".into(), t("3"))],
            set: vec![("i".into(), t("3"), Edit::Text("abc".into()))],
        },
    ];
    let e = d.apply(&o, &changes).await.unwrap_err().to_string();
    assert!(
        e.contains("Change 2 of 2 failed") && e.contains("invalid input syntax for type integer"),
        "{e}"
    );
    assert_eq!(all_rows(&d, "things").await, before);

    // A key that matches two rows: nothing is deleted.
    let del = [Change::Delete {
        key: vec![("a".into(), t("1"))],
    }];
    let e = d.apply(&obj("nokey"), &del).await.unwrap_err().to_string();
    assert!(e.contains("changed 2 rows"), "{e}");
    assert_eq!(d.count(&req("nokey", "", None, 1, 0)).await.unwrap(), 2);

    // The session still works after a rollback.
    assert_eq!(d.apply(&o, &changes[..1]).await.unwrap(), 1);
    assert_eq!(d.count(&req("things", "", None, 1, 0)).await.unwrap(), 4);
}

#[tokio::test]
async fn preview_is_what_runs() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;
    let o = obj("things");
    d.browse(&req("things", "", None, 50, 0)).await.unwrap();

    let preview = d.preview(&o, &edits()).unwrap();
    assert_eq!(
        preview,
        [
            "UPDATE \"it_pg\".\"things\" SET \"name\" = 'O''Brien \\ back\\slash\nnew line 😀', \"note\" = '', \
             \"code\" = NULL, \"b\" = '\\xcafe'::bytea WHERE \"id\" = '1'",
            r#"INSERT INTO "it_pg"."things" ("name", "i") VALUES ('new row', '77')"#,
            r#"DELETE FROM "it_pg"."things" WHERE "id" = '5'"#,
        ]
    );

    // Run the preview text by hand, then apply the same changes to a fresh
    // schema. Both give the same table.
    d.execute(None, &preview.join(";\n"), 10).await.unwrap();
    let by_hand = all_rows(&d, "things").await;
    let d = setup().await;
    d.browse(&req("things", "", None, 50, 0)).await.unwrap();
    d.apply(&o, &edits()).await.unwrap();
    assert_eq!(all_rows(&d, "things").await, by_hand);
}

#[tokio::test]
async fn read_only_connection() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;
    let ro = connect(&ConnConfig {
        read_only: true,
        ..cfg()
    })
    .await
    .unwrap();

    let p = ro.browse(&req("things", "", None, 50, 0)).await.unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("read-only connection"));
    assert_eq!(p.result.rows.len(), 5);

    let e = ro
        .execute(None, "update it_pg.things set name = 'x' where id = 1", 10)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("read-only transaction"), "{e}");
    let e = ro.apply(&obj("things"), &edits()).await.unwrap_err();
    assert!(e.to_string().contains("read-only"), "{e}");
    let r = ro
        .execute(None, "select count(*) from it_pg.things", 10)
        .await
        .unwrap();
    assert_eq!(r[0].rows, vec![texts(&["5"])]);

    let p = d
        .browse(&req("things", "id = 1", None, 50, 0))
        .await
        .unwrap();
    assert_eq!(column(&p, "name"), texts(&["alpha"]));
}

/// Start `select pg_sleep(30)`, cancel it, and check that it stops fast.
async fn check_cancel(d: &Arc<dyn Driver>) {
    let started = Instant::now();
    let d2 = d.clone();
    let task = tokio::spawn(async move { d2.execute(None, "select pg_sleep(30)", 10).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    d.cancel().await;
    let res = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancel took too long")
        .unwrap();
    let e = res.unwrap_err().to_string();
    assert!(e.contains("canceling statement due to user request"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_query() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;
    check_cancel(&d).await;
    // The editor session still works, and the sidebar was never blocked.
    let r = d.execute(None, "select 1", 10).await.unwrap();
    assert_eq!(r[0].rows, vec![texts(&["1"])]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidebar_works_during_long_query() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;
    let d2 = d.clone();
    let task = tokio::spawn(async move { d2.execute(None, "select pg_sleep(3)", 10).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    d.namespaces().await.unwrap();
    d.browse(&req("things", "", None, 50, 0)).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_after_session_dies() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;
    let d = setup().await;

    let r = d
        .execute(None, "select pg_backend_pid()", 10)
        .await
        .unwrap();
    let editor_pid = r[0].rows[0][0].clone();
    let p = d.browse(&req("whoami", "", None, 50, 0)).await.unwrap();
    let meta_pid = p.result.rows[0][0].clone();
    let (Value::Text(e_pid), Value::Text(m_pid)) = (&editor_pid, &meta_pid) else {
        panic!("no pids")
    };

    let killer = connect(&cfg()).await.unwrap();
    killer
        .execute(
            None,
            &format!("select pg_terminate_backend({e_pid}), pg_terminate_backend({m_pid})"),
            10,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Both sessions come back on the next call.
    assert!(d.namespaces().await.unwrap().contains(&"it_pg".to_string()));
    let p = d.browse(&req("whoami", "", None, 50, 0)).await.unwrap();
    assert_ne!(p.result.rows[0][0], meta_pid);
    let r = d
        .execute(None, "select pg_backend_pid()", 10)
        .await
        .unwrap();
    assert_ne!(r[0].rows[0][0], editor_pid);
    // The cancel token follows the new editor session.
    check_cancel(&d).await;
}

#[tokio::test]
async fn tls_modes_and_connect_errors() {
    if !enabled() {
        return;
    }
    let _g = LOCK.lock().await;

    // Off works (also used by every other test).
    let d = connect(&cfg()).await.unwrap();
    d.execute(None, "select 1", 10).await.unwrap();

    // Prefer falls back to plain TCP: this server has no TLS.
    let d = connect(&ConnConfig {
        tls: TlsMode::Prefer,
        ..cfg()
    })
    .await
    .unwrap();
    let r = d
        .execute(
            None,
            "select ssl from pg_stat_ssl where pid = pg_backend_pid()",
            10,
        )
        .await
        .unwrap();
    assert_eq!(r[0].rows, vec![texts(&["f"])]);

    let e = connect(&ConnConfig {
        tls: TlsMode::Require,
        ..cfg()
    })
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(e.contains("server does not support TLS"), "{e}");

    let bad_ca = ConnConfig {
        tls: TlsMode::Verify,
        ca_file: "/nonexistent/ca.pem".into(),
        ..cfg()
    };
    let e = connect(&bad_ca).await.err().unwrap().to_string();
    assert!(e.contains("Cannot read the CA file"), "{e}");

    let e = connect(&ConnConfig {
        password: "wrong".into(),
        ..cfg()
    })
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(e.contains("password authentication failed"), "{e}");

    let e = connect(&ConnConfig { port: 1, ..cfg() })
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("error connecting to server"), "{e}");

    // A URL works too; the password comes from the field.
    let url = ConnConfig {
        url: "postgres://ourdb@127.0.0.1:55432/ourdb?sslmode=verify-full".into(),
        host: String::new(),
        port: 0,
        ..cfg()
    };
    let d = connect(&url).await.unwrap();
    let r = d
        .execute(
            None,
            "select current_database(), current_setting('application_name')",
            10,
        )
        .await
        .unwrap();
    assert_eq!(r[0].rows, vec![texts(&["ourdb", "OurDB"])]);
}
