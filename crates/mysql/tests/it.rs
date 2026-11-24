//! Integration tests against the MySQL 8.0 test server (dev/compose.yml).
//! They run only when OURDB_IT=1: `OURDB_IT=1 cargo test -p ourdb-mysql`.
//! Optional: OURDB_MYSQL_CA=<ca.pem of the server> also tests TLS Verify.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use ourdb_core::{
    BrowseRequest, Change, ConnConfig, Driver, Edit, Kind, ObjectKind, ObjectRef, Page, ResultSet,
    TlsMode, Value, sql::mysql_literal,
};
use tokio::sync::OnceCell;

const DB: &str = "it_my";

fn enabled() -> bool {
    std::env::var("OURDB_IT").is_ok_and(|v| v == "1")
}

fn cfg(user: &str, database: &str) -> ConnConfig {
    ConnConfig {
        kind: Kind::MySql,
        host: "127.0.0.1".into(),
        port: 53306,
        user: user.into(),
        password: "ourdb".into(),
        database: database.into(),
        tls: TlsMode::Prefer,
        ..Default::default()
    }
}

async fn root() -> Arc<dyn Driver> {
    setup().await;
    ourdb_mysql::connect(&cfg("root", DB))
        .await
        .expect("connect as root")
}

static SETUP: OnceCell<()> = OnceCell::const_new();

/// Create the test database once per test run.
async fn setup() {
    SETUP
        .get_or_init(|| async {
            let d = ourdb_mysql::connect(&cfg("root", "")).await.expect("connect for setup");
            let mut sql = String::from(
                r#"
DROP DATABASE IF EXISTS it_my;
CREATE DATABASE it_my CHARACTER SET utf8mb4;
USE it_my;
CREATE TABLE types (
  id INT PRIMARY KEY,
  c_int INT, c_ubig BIGINT UNSIGNED, c_dec DECIMAL(10,2), c_float FLOAT, c_double DOUBLE,
  c_bit BIT(8), c_bool BOOL, c_varchar VARCHAR(50), c_text TEXT, c_blob BLOB, c_varbinary VARBINARY(16),
  c_json JSON, c_enum ENUM('a','b','c'), c_set SET('x','y','z'), c_date DATE, c_datetime DATETIME(3),
  c_timestamp TIMESTAMP NULL, c_time TIME, c_year YEAR, c_emoji TEXT CHARACTER SET utf8mb4
);
INSERT INTO types VALUES
 (1, -5, 18446744073709551615, 12345.67, 1.5, 2.25, b'00000101', TRUE, 'hello', 'long text', X'DEADBEEF',
  X'00FF', '{"a": [1, 2]}', 'b', 'z,x', '2024-02-29', '2024-02-29 13:14:15.123', '2024-02-29 13:14:15',
  '-12:30:00', 2024, 'smile 😀 ok'),
 (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
  NULL, NULL, NULL),
 (3, 0, 0, 0, 0, 0, b'0', FALSE, '', '', X'', X'', '[]', 'a', '', '2000-01-01', '2000-01-01 00:00:00',
  NULL, '00:00:00', 2000, '');
CREATE TABLE people (id INT PRIMARY KEY, name VARCHAR(50), age INT) ENGINE=MyISAM;
CREATE TABLE comp (a INT, b VARCHAR(5), v INT, PRIMARY KEY (b, a)) ENGINE=MyISAM;
INSERT INTO comp VALUES (2, 'x', 1), (1, 'y', 2), (1, 'x', 3);
CREATE VIEW v_people AS SELECT id, name FROM people WHERE age > 30;
CREATE TABLE nopk (a INT, b VARCHAR(10));
INSERT INTO nopk VALUES (1, 'one'), (2, 'two');
CREATE TABLE edits (
  id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(100) NOT NULL DEFAULT 'anon', note TEXT,
  data VARBINARY(32), created TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO edits (id, name, note) VALUES (1, 'first', 'n1'), (2, 'second', 'n2'), (3, 'third', 'n3');
CREATE TABLE edits2 (id INT PRIMARY KEY, name VARCHAR(100));
INSERT INTO edits2 VALUES (1, 'one'), (2, 'two');
"#,
            );
            // MyISAM keeps insert order, so a browse without ORDER BY would not be sorted.
            let ids = [17, 3, 25, 9, 1, 12, 20, 5, 14, 8, 22, 2, 11, 19, 6, 24, 15, 4, 10, 21, 7, 16, 23, 13, 18];
            let rows: Vec<String> = ids.iter().map(|i| format!("({i}, 'p{i}', {})", 18 + (i * 7) % 50)).collect();
            sql.push_str(&format!("INSERT INTO people VALUES {};", rows.join(", ")));
            d.execute(None, &sql, 100).await.expect("setup SQL");
            d.close().await;
        })
        .await;
}

fn req(
    name: &str,
    filter: &str,
    sort: Option<(&str, bool)>,
    limit: u32,
    offset: u64,
) -> BrowseRequest {
    BrowseRequest {
        object: ObjectRef::new(DB, name),
        filter: filter.into(),
        sort: sort.map(|(c, a)| (c.to_string(), a)),
        limit,
        offset,
    }
}

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

/// Values of one column of a page.
fn column(page: &Page, name: &str) -> Vec<Value> {
    let i = page
        .result
        .columns
        .iter()
        .position(|c| c == name)
        .expect("column");
    page.result.rows.iter().map(|r| r[i].clone()).collect()
}

/// The single row of `SELECT ... WHERE id = <id>` as a map-like lookup.
fn cell(rs: &ResultSet, row: usize, name: &str) -> Value {
    let i = rs.columns.iter().position(|c| c == name).expect("column");
    rs.rows[row][i].clone()
}

async fn query(d: &Arc<dyn Driver>, sql: &str) -> Vec<ResultSet> {
    d.execute(Some(DB), sql, 10_000)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[tokio::test]
async fn metadata() {
    if !enabled() {
        return;
    }
    let d = root().await;
    assert_eq!(d.kind(), Kind::MySql);
    assert_eq!(d.default_namespace().as_deref(), Some(DB));
    assert!(d.namespaces().await.unwrap().contains(&DB.to_string()));

    let objects = d.objects(DB).await.unwrap();
    let names: Vec<&str> = objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "comp", "edits", "edits2", "nopk", "people", "types", "v_people"
        ]
    );
    let kind = |n: &str| objects.iter().find(|o| o.name == n).unwrap().kind;
    assert_eq!(kind("people"), ObjectKind::Table);
    assert_eq!(kind("v_people"), ObjectKind::View);

    let cols = d.columns(&ObjectRef::new(DB, "people")).await.unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "age"]);
    assert!(cols[0].key && !cols[0].nullable);
    assert!(!cols[1].key && cols[1].nullable);
    assert_eq!(cols[0].type_name, "int");
    assert_eq!(cols[1].type_name, "varchar(50)");

    let cols = d.columns(&ObjectRef::new(DB, "comp")).await.unwrap();
    let keys: Vec<bool> = cols.iter().map(|c| c.key).collect();
    assert_eq!(keys, [true, true, false]);

    let cols = d.columns(&ObjectRef::new(DB, "edits")).await.unwrap();
    let name = cols.iter().find(|c| c.name == "name").unwrap();
    assert_eq!(name.default.as_deref(), Some("anon"));
    assert!(!name.nullable);
    assert_eq!(
        cols.iter().find(|c| c.name == "note").unwrap().default,
        None
    );

    let cols = d.columns(&ObjectRef::new(DB, "v_people")).await.unwrap();
    assert_eq!(cols.len(), 2);
    assert!(cols.iter().all(|c| !c.key));
    d.close().await;
}

#[tokio::test]
async fn browse_and_count() {
    if !enabled() {
        return;
    }
    let d = root().await;
    let ids = |p: &Page| {
        column(p, "id")
            .iter()
            .map(|v| v.edit_text().parse::<i64>().unwrap())
            .collect::<Vec<_>>()
    };

    // Default order is the key, also on a MyISAM table that keeps insert order.
    let p = d.browse(&req("people", "", None, 10, 0)).await.unwrap();
    assert_eq!(p.key, ["id"]);
    assert_eq!(p.read_only_reason, None);
    assert_eq!(p.result.columns, ["id", "name", "age"]);
    assert_eq!(p.result.types, ["int", "varchar", "int"]);
    assert_eq!(ids(&p), (1..=10).collect::<Vec<_>>());
    let p = d.browse(&req("people", "", None, 10, 20)).await.unwrap();
    assert_eq!(ids(&p), (21..=25).collect::<Vec<_>>());

    // Filter and sort.
    let p = d
        .browse(&req("people", "age > 40", Some(("age", false)), 100, 0))
        .await
        .unwrap();
    let ages: Vec<i64> = column(&p, "age")
        .iter()
        .map(|v| v.edit_text().parse().unwrap())
        .collect();
    assert!(!ages.is_empty());
    assert!(ages.iter().all(|a| *a > 40));
    assert!(ages.windows(2).all(|w| w[0] >= w[1]));
    let p = d
        .browse(&req(
            "people",
            "name LIKE 'p1%'",
            Some(("name", true)),
            100,
            0,
        ))
        .await
        .unwrap();
    assert_eq!(column(&p, "name")[..3], [t("p1"), t("p10"), t("p11")]);

    // A trailing comment in the filter does not break the statement.
    let p = d
        .browse(&req("people", "id <= 3 -- first ones", None, 100, 0))
        .await
        .unwrap();
    assert_eq!(ids(&p), [1, 2, 3]);
    let p = d
        .browse(&req("people", "id <= 2 # mysql comment", None, 100, 0))
        .await
        .unwrap();
    assert_eq!(ids(&p), [1, 2]);

    // Composite key: index order (b, a), and the grid sorts by it.
    let p = d.browse(&req("comp", "", None, 10, 0)).await.unwrap();
    assert_eq!(p.key, ["b", "a"]);
    assert_eq!(column(&p, "v"), [t("3"), t("1"), t("2")]);

    // Views and tables without a key are read-only.
    let p = d.browse(&req("v_people", "", None, 10, 0)).await.unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("view"));
    let p = d.browse(&req("nopk", "", None, 10, 0)).await.unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("no primary key"));
    assert_eq!(p.result.rows.len(), 2);

    assert_eq!(d.count(&req("people", "", None, 10, 0)).await.unwrap(), 25);
    assert_eq!(
        d.count(&req("people", "id > 20", Some(("age", true)), 1, 5))
            .await
            .unwrap(),
        5
    );

    // A bad filter gives the server message.
    let err = d
        .browse(&req("people", "nope = 1", None, 10, 0))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("1054") && err.contains("nope"), "{err}");
    d.close().await;
}

#[tokio::test]
async fn value_types() {
    if !enabled() {
        return;
    }
    let d = root().await;
    let p = d.browse(&req("types", "", None, 10, 0)).await.unwrap();
    let rs = &p.result;
    assert_eq!(rs.rows.len(), 3);
    let expect = [
        ("c_int", t("-5")),
        ("c_ubig", t("18446744073709551615")),
        ("c_dec", t("12345.67")),
        ("c_float", t("1.5")),
        ("c_double", t("2.25")),
        ("c_bit", Value::Bytes(vec![5])),
        ("c_bool", t("1")),
        ("c_varchar", t("hello")),
        ("c_text", t("long text")),
        ("c_blob", Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])),
        ("c_varbinary", Value::Bytes(vec![0x00, 0xff])),
        ("c_json", t(r#"{"a": [1, 2]}"#)),
        ("c_enum", t("b")),
        ("c_set", t("x,z")),
        ("c_date", t("2024-02-29")),
        ("c_datetime", t("2024-02-29 13:14:15.123")),
        ("c_timestamp", t("2024-02-29 13:14:15")),
        ("c_time", t("-12:30:00")),
        ("c_year", t("2024")),
        ("c_emoji", t("smile 😀 ok")),
    ];
    for (name, value) in &expect {
        assert_eq!(&cell(rs, 0, name), value, "{name}");
        // Row 2 is all NULL.
        assert_eq!(cell(rs, 1, name), Value::Null, "{name}");
    }
    // Empty strings and empty bytes are not NULL.
    assert_eq!(cell(rs, 2, "c_varchar"), t(""));
    assert_eq!(cell(rs, 2, "c_text"), t(""));
    assert_eq!(cell(rs, 2, "c_blob"), Value::Bytes(vec![]));
    assert_eq!(cell(rs, 2, "c_varbinary"), Value::Bytes(vec![]));
    assert_eq!(cell(rs, 2, "c_set"), t(""));

    let ty = |name: &str| rs.types[rs.columns.iter().position(|c| c == name).unwrap()].clone();
    assert_eq!(ty("c_ubig"), "bigint unsigned");
    assert_eq!(ty("c_blob"), "blob");
    assert_eq!(ty("c_text"), "text");
    assert_eq!(ty("c_varbinary"), "varbinary");
    assert_eq!(ty("c_json"), "json");
    assert_eq!(ty("c_enum"), "enum");
    assert_eq!(ty("c_year"), "year");
    assert_eq!(ty("c_bit"), "bit");

    // Expressions: binary strings are bytes, other strings are text.
    let sets = query(
        &d,
        "SELECT X'4142' AS b, 'AB' AS s, 1 + 1 AS n, CAST('x' AS BINARY) AS c",
    )
    .await;
    assert_eq!(
        sets[0].rows[0],
        [
            Value::Bytes(b"AB".to_vec()),
            t("AB"),
            t("2"),
            Value::Bytes(b"x".to_vec())
        ]
    );
    d.close().await;
}

#[tokio::test]
async fn execute_statements() {
    if !enabled() {
        return;
    }
    let d = root().await;
    assert!(d.execute(None, "  ;  ", 10).await.unwrap().is_empty());

    let sets = query(
        &d,
        "SELECT 1 AS a, 'x' AS b; UPDATE nopk SET b = b WHERE a = -1; # it's a comment\n\
         SELECT id FROM people ORDER BY id LIMIT 3",
    )
    .await;
    assert_eq!(sets.len(), 3);
    assert_eq!(sets[0].columns, ["a", "b"]);
    assert_eq!(sets[0].rows, [[t("1"), t("x")]]);
    assert_eq!(sets[1].affected, Some(0));
    assert!(sets[1].columns.is_empty());
    assert!(
        sets[1]
            .message
            .as_deref()
            .unwrap()
            .starts_with("0 rows affected")
    );
    assert_eq!(sets[2].rows.len(), 3);

    // Row cap.
    let sets = d
        .execute(Some(DB), "SELECT * FROM people", 10)
        .await
        .unwrap();
    assert_eq!(sets[0].rows.len(), 10);
    assert!(sets[0].truncated);
    let sets = d
        .execute(Some(DB), "SELECT * FROM people", 25)
        .await
        .unwrap();
    assert!(!sets[0].truncated);

    // Errors name the statement.
    let err = d
        .execute(
            Some(DB),
            "SELECT 1; SELECT * FROM no_such_table; SELECT 3",
            10,
        )
        .await
        .unwrap_err();
    let err = err.to_string();
    assert!(err.starts_with("Statement 2: ERROR 1146"), "{err}");
    let err = d
        .execute(Some(DB), "SELECT * FROM no_such_table", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("ERROR 1146 (42S02)"), "{err}");
    let err = d
        .execute(Some(DB), "SELEC 1", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("1064"), "{err}");

    // The session persists between runs, and the connection survives errors.
    query(
        &d,
        "SET @v = 42; CREATE TEMPORARY TABLE tmp_x (a INT); INSERT INTO tmp_x VALUES (7)",
    )
    .await;
    let sets = query(&d, "SELECT @v, (SELECT a FROM tmp_x)").await;
    assert_eq!(sets[0].rows, [[t("42"), t("7")]]);

    // The namespace runs USE first; a user USE in the text also persists.
    let sets = d
        .execute(Some("mysql"), "SELECT DATABASE()", 10)
        .await
        .unwrap();
    assert_eq!(sets[0].rows, [[t("mysql")]]);
    let sets = d
        .execute(None, "USE it_my; SELECT COUNT(*) FROM people", 10)
        .await
        .unwrap();
    assert_eq!(sets[1].rows, [[t("25")]]);
    let sets = d.execute(None, "SELECT DATABASE()", 10).await.unwrap();
    assert_eq!(sets[0].rows, [[t(DB)]]);
    let err = d
        .execute(Some("no_such_db"), "SELECT 1", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("1049"), "{err}");

    // Transactions persist between runs too.
    query(&d, "BEGIN; UPDATE nopk SET b = 'temp' WHERE a = 1").await;
    query(&d, "ROLLBACK").await;
    let sets = query(&d, "SELECT b FROM nopk WHERE a = 1").await;
    assert_eq!(sets[0].rows, [[t("one")]]);
    d.close().await;
}

#[tokio::test]
async fn apply_edits() {
    if !enabled() {
        return;
    }
    let d = root().await;
    let obj = ObjectRef::new(DB, "edits");
    let tricky = "O'Brien \\ back\\slash \"q\"\nline\r\ttab 😀 %_ \\n";

    // Browse first: it tells the driver which columns are binary.
    let p = d.browse(&req("edits", "", None, 100, 0)).await.unwrap();
    assert_eq!(p.key, ["id"]);
    let key = |id: &str| vec![("id".to_string(), t(id))];
    let changes = vec![
        Change::Update {
            key: key("1"),
            set: vec![
                ("name".into(), t("first"), Edit::Text(tricky.into())),
                ("note".into(), t("n1"), Edit::Null),
                ("data".into(), Value::Null, Edit::Text("0xCAFE00".into())),
            ],
        },
        Change::Insert { values: vec![] },
        Change::Insert {
            values: vec![
                ("id".into(), Edit::Text("100".into())),
                ("note".into(), Edit::Text(tricky.into())),
            ],
        },
        Change::Delete { key: key("2") },
    ];
    let preview = d.preview(&obj, &changes).unwrap();
    assert_eq!(preview.len(), 4);
    assert!(preview[0].contains("`data` = X'CAFE00'"), "{}", preview[0]);
    assert_eq!(preview[1], "INSERT INTO `it_my`.`edits` () VALUES ()");
    assert_eq!(d.apply(&obj, &changes).await.unwrap(), 4);

    // The server ran exactly the preview text (per-thread statement history).
    for stmt in &preview {
        let sql = format!(
            "SELECT COUNT(*) FROM performance_schema.events_statements_history WHERE SQL_TEXT = {} COLLATE utf8mb4_bin",
            mysql_literal(stmt, true)
        );
        let sets = query(&d, &sql).await;
        assert_eq!(sets[0].rows[0][0], t("1"), "not run verbatim: {stmt}");
    }

    let p = d.browse(&req("edits", "", None, 100, 0)).await.unwrap();
    let rs = &p.result;
    let ids: Vec<Value> = column(&p, "id");
    assert_eq!(ids[0], t("1"));
    assert!(!ids.contains(&t("2")), "row 2 deleted");
    assert!(ids.contains(&t("100")));
    assert_eq!(cell(rs, 0, "name"), t(tricky));
    assert_eq!(cell(rs, 0, "note"), Value::Null);
    assert_eq!(cell(rs, 0, "data"), Value::Bytes(vec![0xca, 0xfe, 0x00]));
    // The empty insert got the defaults.
    let defaults = rs
        .rows
        .iter()
        .position(|r| r[0] != t("1") && r[0] != t("3") && r[0] != t("100"))
        .unwrap();
    assert_eq!(cell(rs, defaults, "name"), t("anon"));
    assert_eq!(cell(rs, defaults, "note"), Value::Null);
    assert!(!cell(rs, defaults, "created").is_null());
    let row100 = ids.iter().position(|v| *v == t("100")).unwrap();
    assert_eq!(cell(rs, row100, "note"), t(tricky));
    assert_eq!(cell(rs, row100, "name"), t("anon"));

    // Edit the bytes again from their shown text, and write an empty string.
    let shown = cell(rs, 0, "data").edit_text();
    assert_eq!(shown, "0xcafe00");
    let changes = vec![Change::Update {
        key: key("1"),
        set: vec![
            ("data".into(), cell(rs, 0, "data"), Edit::Text("0x".into())),
            ("note".into(), Value::Null, Edit::Text(String::new())),
        ],
    }];
    d.apply(&obj, &changes).await.unwrap();
    let sets = query(&d, "SELECT data, note FROM edits WHERE id = 1").await;
    assert_eq!(sets[0].rows, [[Value::Bytes(vec![]), t("")]]);

    // Bad hex input is refused before anything runs.
    let bad = vec![Change::Update {
        key: key("1"),
        set: vec![("data".into(), Value::Null, Edit::Text("0xZZ".into()))],
    }];
    assert!(d.preview(&obj, &bad).is_err());
    assert!(d.apply(&obj, &bad).await.is_err());
    d.close().await;
}

#[tokio::test]
async fn exactly_one_row() {
    if !enabled() {
        return;
    }
    let d = root().await;
    let obj = ObjectRef::new(DB, "edits2");
    let key = |id: &str| vec![("id".to_string(), t(id))];
    let set = |v: &str| vec![("name".to_string(), t("old"), Edit::Text(v.into()))];

    // The second change hits no row: the first one rolls back too.
    let changes = vec![
        Change::Update {
            key: key("1"),
            set: set("changed"),
        },
        Change::Update {
            key: key("999"),
            set: set("ghost"),
        },
    ];
    let err = d.apply(&obj, &changes).await.unwrap_err().to_string();
    assert!(
        err.contains("Change 2") && err.contains("exactly 1") && err.contains("Nothing was saved"),
        "{err}"
    );
    let sets = query(&d, "SELECT name FROM edits2 WHERE id = 1").await;
    assert_eq!(sets[0].rows, [[t("one")]]);

    let err = d
        .apply(&obj, &[Change::Delete { key: key("999") }])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("affected 0 rows"), "{err}");

    // A server error also rolls back everything.
    let changes = vec![
        Change::Delete { key: key("2") },
        Change::Insert {
            values: vec![("id".into(), Edit::Text("1".into()))],
        },
    ];
    let err = d.apply(&obj, &changes).await.unwrap_err().to_string();
    assert!(err.contains("Change 2") && err.contains("1062"), "{err}");
    assert_eq!(d.count(&req("edits2", "", None, 10, 0)).await.unwrap(), 2);

    // An UPDATE to the same value still counts as one row.
    assert_eq!(
        d.apply(
            &obj,
            &[Change::Update {
                key: key("1"),
                set: set("one")
            }]
        )
        .await
        .unwrap(),
        1
    );
    d.close().await;
}

#[tokio::test]
async fn read_only() {
    if !enabled() {
        return;
    }
    setup().await;
    let mut c = cfg("root", DB);
    c.read_only = true;
    let d = ourdb_mysql::connect(&c).await.unwrap();

    let p = d.browse(&req("people", "", None, 5, 0)).await.unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("read-only connection"));
    assert_eq!(p.result.rows.len(), 5);

    let err = d
        .execute(None, "INSERT INTO nopk VALUES (9, 'nine')", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("1792"), "{err}");
    let err = d
        .execute(None, "CREATE TABLE ro_new (a INT)", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("1792"), "{err}");
    let change = Change::Delete {
        key: vec![("id".into(), t("1"))],
    };
    let err = d
        .apply(&ObjectRef::new(DB, "edits2"), &[change])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("read-only"), "{err}");
    assert_eq!(d.count(&req("nopk", "", None, 10, 0)).await.unwrap(), 2);
    d.close().await;
}

#[tokio::test]
async fn cancel_sleep() {
    if !enabled() {
        return;
    }
    let d = root().await;
    // Cancel with nothing running does nothing.
    d.cancel().await;
    let runner = d.clone();
    let start = Instant::now();
    let task = tokio::spawn(async move { runner.execute(None, "SELECT SLEEP(30)", 10).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    d.cancel().await;
    let out = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancel was too slow")
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    // SLEEP() returns 1 when it is interrupted; other queries give error 1317.
    match out {
        Ok(sets) => assert_eq!(sets[0].rows, [[t("1")]]),
        Err(e) => assert!(e.to_string().contains("1317"), "{e}"),
    }
    // The editor connection still works.
    let sets = query(&d, "SELECT 2").await;
    assert_eq!(sets[0].rows, [[t("2")]]);

    // A query that is not a lone SLEEP fails with 1317.
    let runner = d.clone();
    let task = tokio::spawn(async move {
        runner
            .execute(
                None,
                "SELECT SLEEP(30), 1 FROM DUAL WHERE SLEEP(30) = 0",
                10,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    d.cancel().await;
    let out = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancel was too slow")
        .unwrap();
    if let Err(e) = out {
        assert!(e.to_string().contains("1317"), "{e}");
    }
    d.close().await;
}

#[tokio::test]
async fn login_as_user() {
    if !enabled() {
        return;
    }
    let r = root().await;
    for tls in [TlsMode::Off, TlsMode::Prefer] {
        // Empty the caching_sha2_password cache: the next login does the full auth.
        query(&r, "FLUSH PRIVILEGES").await;
        let mut c = cfg("ourdb", "ourdb");
        c.tls = tls;
        let d = ourdb_mysql::connect(&c)
            .await
            .unwrap_or_else(|e| panic!("{tls:?}: {e}"));
        assert_eq!(d.default_namespace().as_deref(), Some("ourdb"));
        assert_eq!(
            d.namespaces().await.unwrap(),
            ["information_schema", "ourdb", "performance_schema"]
        );
        let sets = d.execute(None, "SELECT CURRENT_USER()", 10).await.unwrap();
        assert_eq!(sets[0].rows, [[t("ourdb@%")]]);
        d.close().await;
    }
    let mut c = cfg("ourdb", "ourdb");
    c.password = "wrong".into();
    let err = ourdb_mysql::connect(&c)
        .await
        .err()
        .expect("bad password must fail")
        .to_string();
    assert!(err.contains("1045"), "{err}");
    r.close().await;
}

async fn ssl_cipher(d: &Arc<dyn Driver>) -> String {
    let sets = d
        .execute(None, "SHOW SESSION STATUS LIKE 'Ssl_cipher'", 10)
        .await
        .unwrap();
    sets[0].rows[0][1].edit_text()
}

#[tokio::test]
async fn tls_modes() {
    if !enabled() {
        return;
    }
    setup().await;
    for (tls, secure) in [
        (TlsMode::Off, false),
        (TlsMode::Prefer, true),
        (TlsMode::Require, true),
    ] {
        let mut c = cfg("root", DB);
        c.tls = tls;
        let d = ourdb_mysql::connect(&c)
            .await
            .unwrap_or_else(|e| panic!("{tls:?}: {e}"));
        assert_eq!(!ssl_cipher(&d).await.is_empty(), secure, "{tls:?}");
        // Pool connections use the same TLS setting.
        let sets = d.browse(&req("people", "", None, 1, 0)).await.unwrap();
        assert_eq!(sets.result.rows.len(), 1);
        d.close().await;
    }

    // Verify fails: the server certificate is self-signed.
    let mut c = cfg("root", DB);
    c.tls = TlsMode::Verify;
    let err = ourdb_mysql::connect(&c)
        .await
        .err()
        .expect("verify must fail")
        .to_string();
    assert!(err.contains("TLS"), "{err}");

    c.ca_file = "/no/such/ca.pem".into();
    let err = ourdb_mysql::connect(&c)
        .await
        .err()
        .expect("missing CA file must fail")
        .to_string();
    assert!(err.contains("CA file"), "{err}");

    // With the server CA, Verify works when the host name check is skipped (tunnel).
    // Note: macOS refuses server certificates valid for more than 825 days, and
    // the certificates that MySQL makes itself are valid for 10 years.
    if let Ok(ca) = std::env::var("OURDB_MYSQL_CA") {
        c.ca_file = ca;
        c.via_tunnel = true;
        let d = ourdb_mysql::connect(&c).await.unwrap();
        assert!(!ssl_cipher(&d).await.is_empty());
        d.close().await;
    }
}

/// A TCP proxy to the test server that removes CLIENT_SSL from the server
/// greeting, like a server without TLS. Returns the local port.
async fn no_tls_proxy() -> u16 {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut server = TcpStream::connect("127.0.0.1:53306").await.unwrap();
                let mut head = [0u8; 4];
                server.read_exact(&mut head).await.unwrap();
                let mut body =
                    vec![0u8; u32::from_le_bytes([head[0], head[1], head[2], 0]) as usize];
                server.read_exact(&mut body).await.unwrap();
                // Protocol version, version text + NUL, thread id (4), salt (8), filler (1), capabilities.
                let version_end = 1 + body[1..].iter().position(|b| *b == 0).unwrap();
                let caps = version_end + 1 + 4 + 8 + 1;
                body[caps + 1] &= !0x08; // CLIENT_SSL = 0x0800
                client.write_all(&head).await.unwrap();
                client.write_all(&body).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_prefer_falls_back() {
    if !enabled() {
        return;
    }
    setup().await;
    let port = no_tls_proxy().await;
    let mut c = cfg("root", DB);
    c.port = port;
    let d = ourdb_mysql::connect(&c).await.unwrap();
    assert!(ssl_cipher(&d).await.is_empty());
    assert_eq!(d.count(&req("people", "", None, 1, 0)).await.unwrap(), 25);
    d.close().await;

    c.tls = TlsMode::Require;
    let err = ourdb_mysql::connect(&c)
        .await
        .err()
        .expect("require must fail without TLS")
        .to_string();
    assert!(err.contains("SSL"), "{err}");
}

#[tokio::test]
async fn url_config() {
    if !enabled() {
        return;
    }
    setup().await;
    let c = ConnConfig {
        kind: Kind::MySql,
        url: "mysql://root@127.0.0.1:53306/it_my".into(),
        password: "ourdb".into(),
        tls: TlsMode::Require,
        ..Default::default()
    };
    let d = ourdb_mysql::connect(&c).await.unwrap();
    assert_eq!(d.default_namespace().as_deref(), Some(DB));
    assert_eq!(d.count(&req("people", "", None, 1, 0)).await.unwrap(), 25);
    d.close().await;

    let err = ourdb_mysql::connect(&ConnConfig {
        port: 1,
        ..cfg("root", "")
    })
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(
        err.starts_with("Cannot connect to MySQL at 127.0.0.1:1"),
        "{err}"
    );
}
