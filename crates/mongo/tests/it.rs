//! Integration tests against the dev MongoDB (dev/compose.yml).
//! Run with: OURDB_IT=1 cargo test -p ourdb-mongo
//!
//! The server tests share the database `it_mongo`, so they run in order in
//! one test function.

use std::sync::Arc;

use mongodb::bson::{Bson, DateTime, Document, doc, oid::ObjectId};
use ourdb_core::{
    BrowseRequest, Change, ConnConfig, Driver, Edit, Kind, ObjectKind, ObjectRef, Page, TlsMode,
    Value,
};
use serde_json::json;

const DB: &str = "it_mongo";

fn enabled() -> bool {
    std::env::var("OURDB_IT").as_deref() == Ok("1")
}

fn config(kind: Kind) -> ConnConfig {
    ConnConfig {
        kind,
        host: "127.0.0.1".into(),
        port: 57017,
        user: "ourdb".into(),
        password: "ourdb".into(),
        database: DB.into(),
        tls: TlsMode::Off,
        ..ConnConfig::default()
    }
}

/// A plain driver client, to check the stored BSON types.
async fn raw() -> mongodb::Database {
    let client =
        mongodb::Client::with_uri_str("mongodb://ourdb:ourdb@127.0.0.1:57017/?authSource=admin")
            .await
            .unwrap();
    client.database(DB)
}

async fn raw_doc(coll: &str, id: &str) -> Document {
    let oid = ObjectId::parse_str(id).unwrap();
    raw()
        .await
        .collection::<Document>(coll)
        .find_one(doc! {"_id": oid})
        .await
        .unwrap()
        .unwrap()
}

fn oid_value(hex: &str) -> Value {
    Value::Json(json!({"$oid": hex}))
}

fn obj(name: &str) -> ObjectRef {
    ObjectRef::new(DB, name)
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
        sort: sort.map(|(c, a)| (c.to_string(), a)),
        limit,
        offset,
    }
}

fn col(page: &Page, name: &str) -> Vec<Value> {
    let i = page
        .result
        .columns
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no column {name}"));
    page.result.rows.iter().map(|r| r[i].clone()).collect()
}

fn texts(values: &[Value]) -> Vec<String> {
    values.iter().map(Value::display).collect()
}

async fn run(d: &Arc<dyn Driver>, text: &str) -> Vec<ourdb_core::ResultSet> {
    d.execute(Some(DB), text, 1000)
        .await
        .unwrap_or_else(|e| panic!("{text}\n-> {e}"))
}

async fn run_err(d: &Arc<dyn Driver>, text: &str) -> String {
    let e = d
        .execute(Some(DB), text, 1000)
        .await
        .expect_err(text)
        .to_string();
    eprintln!("error for {text:?}:\n  {e}");
    e
}

#[tokio::test]
async fn srv_url_through_tunnel_is_refused() {
    // No server needed: the driver refuses before any DNS lookup.
    let cfg = ConnConfig {
        url: "mongodb+srv://cluster.example.com/db".into(),
        via_tunnel: true,
        ..config(Kind::MongoDb)
    };
    let e = ourdb_mongo::connect(&cfg)
        .await
        .err()
        .expect("must fail")
        .to_string();
    assert!(
        e.contains("mongodb+srv://") && e.contains("SSH tunnel"),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mongo_driver() {
    if !enabled() {
        return;
    }
    let d = ourdb_mongo::connect(&config(Kind::MongoDb)).await.unwrap();
    assert_eq!(d.kind(), Kind::MongoDb);
    raw().await.drop().await.unwrap();
    seed(&d).await;
    metadata(&d).await;
    browse(&d).await;
    execute(&d).await;
    typed_edits(&d).await;
    documents(&d).await;
    exactly_one(&d).await;
    read_only().await;
    document_db().await;
    url_and_login().await;
    d.close().await;
}

async fn seed(d: &Arc<dyn Driver>) {
    let rs = run(
        d,
        "db.users.insertMany([
           {_id: ObjectId('650000000000000000000001'), name: 'Ann', age: 31, tags: ['a', 'b'], joined: ISODate('2024-01-02T03:04:05Z')},
           {_id: ObjectId('650000000000000000000002'), name: 'Bob', age: 25, email: 'bob@x.io'},
           {_id: ObjectId('650000000000000000000003'), name: 'Cid', age: 40, nested: {x: 1}},
           {_id: ObjectId('650000000000000000000004'), name: 'Dee', age: 35, score: NumberLong(10)},
           {_id: ObjectId('650000000000000000000005'), name: 'eve', age: null},
         ])
         db.runCommand({create: 'users_view', viewOn: 'users', pipeline: [{$match: {age: {$gt: 30}}}]})",
    )
    .await;
    assert_eq!(rs[0].affected, Some(5));
    assert_eq!(rs[0].message.as_deref(), Some("inserted 5 documents"));
}

async fn metadata(d: &Arc<dyn Driver>) {
    assert!(d.namespaces().await.unwrap().contains(&DB.to_string()));
    assert_eq!(d.default_namespace().as_deref(), Some(DB));
    let objects = d.objects(DB).await.unwrap();
    let names: Vec<&str> = objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        ["users", "users_view"],
        "system.views must be hidden"
    );
    assert_eq!(objects[0].kind, ObjectKind::Collection);
    assert_eq!(objects[1].kind, ObjectKind::View);

    let cols = d.columns(&obj("users")).await.unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "_id", "name", "age", "tags", "joined", "email", "nested", "score"
        ]
    );
    let get = |n: &str| cols.iter().find(|c| c.name == n).unwrap();
    assert_eq!(
        (
            get("_id").type_name.as_str(),
            get("_id").key,
            get("_id").nullable
        ),
        ("ObjectId", true, false)
    );
    assert_eq!(
        (get("name").type_name.as_str(), get("name").nullable),
        ("String", false)
    );
    assert_eq!(
        (get("age").type_name.as_str(), get("age").nullable),
        ("Int32", true)
    );
    assert_eq!(get("joined").type_name, "Date");
    assert_eq!(get("score").type_name, "Int64");
    assert!(get("email").nullable && !get("email").key);
}

async fn browse(d: &Arc<dyn Driver>) {
    // Default order is _id; pages are server-side.
    let p = d.browse(&req("users", "", None, 2, 0)).await.unwrap();
    assert_eq!(p.key, ["_id"]);
    assert_eq!(p.read_only_reason, None);
    assert_eq!(
        p.result.columns,
        ["_id", "name", "age", "tags", "joined", "email"]
    );
    assert_eq!(p.result.types[0], "ObjectId");
    assert_eq!(texts(&col(&p, "name")), ["Ann", "Bob"]);
    assert_eq!(col(&p, "email")[0], Value::Missing);
    assert_eq!(col(&p, "email")[1], Value::Text("bob@x.io".into()));
    assert_eq!(col(&p, "_id")[0], oid_value("650000000000000000000001"));
    assert_eq!(col(&p, "joined")[0].display(), "2024-01-02T03:04:05.000Z");
    let p2 = d.browse(&req("users", "", None, 2, 2)).await.unwrap();
    assert_eq!(texts(&col(&p2, "name")), ["Cid", "Dee"]);
    assert_eq!(p2.result.columns, ["_id", "name", "age", "nested", "score"]);

    // Shell-syntax filters.
    let p = d
        .browse(&req(
            "users",
            "{_id: ObjectId(\"650000000000000000000003\")}",
            None,
            50,
            0,
        ))
        .await
        .unwrap();
    assert_eq!(texts(&col(&p, "name")), ["Cid"]);
    let p = d
        .browse(&req(
            "users",
            "{age: {$gt: 30}}",
            Some(("age", false)),
            50,
            0,
        ))
        .await
        .unwrap();
    assert_eq!(texts(&col(&p, "name")), ["Cid", "Dee", "Ann"]);
    let p = d
        .browse(&req(
            "users",
            "{name: {$regex: '^[a-d]', $options: 'i'}}",
            None,
            50,
            0,
        ))
        .await
        .unwrap();
    assert_eq!(texts(&col(&p, "name")), ["Ann", "Bob", "Cid", "Dee"]);
    let p = d
        .browse(&req("users", "", Some(("name", false)), 50, 0))
        .await
        .unwrap();
    assert_eq!(texts(&col(&p, "name")), ["eve", "Dee", "Cid", "Bob", "Ann"]);
    assert_eq!(col(&p, "age")[0], Value::Null);
    // Empty result keeps the _id column.
    let p = d
        .browse(&req("users", "{name: 'nobody'}", None, 50, 0))
        .await
        .unwrap();
    assert_eq!(p.result.columns, ["_id"]);
    assert!(p.result.rows.is_empty());
    // Views are read-only.
    let p = d.browse(&req("users_view", "", None, 50, 0)).await.unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("view"));
    assert_eq!(p.result.rows.len(), 3);

    let e = d
        .browse(&req("users", "{age: {$gt: }}", None, 50, 0))
        .await
        .unwrap_err()
        .to_string();
    eprintln!("bad filter: {e}");
    assert!(
        e.starts_with("filter: syntax error") && e.contains("{age: {$gt: }}"),
        "{e}"
    );

    assert_eq!(d.count(&req("users", "", None, 50, 0)).await.unwrap(), 5);
    assert_eq!(
        d.count(&req("users", "{age: {$gt: 30}}", None, 1, 3))
            .await
            .unwrap(),
        3
    );
}

async fn execute(d: &Arc<dyn Driver>) {
    // A find chain over several lines, then two counts in one run.
    let rs = run(
        d,
        "db.users.find({age: {$gte: 30}}, {name: 1, age: 1})
           .sort({age: -1})
           // a comment between chained calls
           .limit(2)
         db.users.countDocuments({age: {$gt: 30}}); db.users.estimatedDocumentCount()",
    )
    .await;
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[0].columns, ["_id", "name", "age"]);
    assert_eq!(
        rs[0]
            .rows
            .iter()
            .map(|r| r[1].display())
            .collect::<Vec<_>>(),
        ["Cid", "Dee"]
    );
    assert!(!rs[0].truncated);
    assert_eq!(
        (rs[1].columns.clone(), rs[1].rows.clone()),
        (vec!["count".to_string()], vec![vec![Value::Int(3)]])
    );
    assert_eq!(rs[2].rows, vec![vec![Value::Int(5)]]);

    // Row cap.
    let rs = d.execute(Some(DB), "db.users.find()", 2).await.unwrap();
    assert_eq!(rs[0].rows.len(), 2);
    assert!(rs[0].truncated);

    let rs = run(d, "db.users.aggregate([{$match: {age: {$ne: null}}}, {$group: {_id: null, total: {$sum: '$age'}}}])").await;
    assert_eq!(rs[0].columns, ["_id", "total"]);
    assert_eq!(rs[0].rows, vec![vec![Value::Null, Value::Int(131)]]);

    let rs = run(d, "db.users.distinct('name', {age: {$gt: 30}})").await;
    assert_eq!(rs[0].columns, ["name"]);
    assert_eq!(rs[0].rows.len(), 3);

    let rs = run(d, "db.getCollection('things').insertOne({_id: 1, a: 1})").await;
    assert_eq!(rs[0].affected, Some(1));
    assert_eq!(rs[0].message.as_deref(), Some("inserted 1 document, _id 1"));
    let rs = run(
        d,
        "db.users.updateMany({age: {$gt: 30}}, {$set: {senior: true}})",
    )
    .await;
    assert_eq!(rs[0].affected, Some(3));
    assert_eq!(rs[0].message.as_deref(), Some("matched 3, modified 3"));
    let rs = run(
        d,
        "db.users.updateMany({age: {$gt: 30}}, {$unset: {senior: ''}})",
    )
    .await;
    assert_eq!(rs[0].message.as_deref(), Some("matched 3, modified 3"));
    let rs = run(
        d,
        "db.things.insertMany([{a: 2}, {a: 3}]); db.things.deleteMany({a: {$gte: 2}})",
    )
    .await;
    assert_eq!(rs[1].affected, Some(2));
    assert_eq!(rs[1].message.as_deref(), Some("deleted 2 documents"));
    let rs = run(
        d,
        "db.things.updateOne({_id: 9}, {$set: {a: 9}}, {upsert: true})",
    )
    .await;
    assert_eq!(
        rs[0].message.as_deref(),
        Some("matched 0, modified 0, upserted _id 9")
    );

    // runCommand keeps key order: the command name must come first.
    let rs = run(d, "db.runCommand({ping: 1})").await;
    assert_eq!(rs[0].columns, ["ok"]);
    assert_eq!(rs[0].rows, vec![vec![Value::Float(1.0)]]);
    let rs = run(d, "db.runCommand({find: 'users', filter: {name: 'Ann'}})").await;
    assert!(rs[0].columns.contains(&"cursor".to_string()));
    assert!(
        rs[0].rows[0][0].display().contains("Ann"),
        "{:?}",
        rs[0].rows
    );

    let rs = run(d, "show dbs\nshow collections").await;
    assert!(rs[0].rows.iter().any(|r| r[0] == Value::Text(DB.into())));
    assert_eq!(rs[0].columns, ["name", "sizeOnDisk", "empty"]);
    let names: Vec<String> = rs[1].rows.iter().map(|r| r[0].display()).collect();
    assert_eq!(names, ["things", "users", "users_view"]);

    // Errors name the statement. A syntax error runs nothing.
    let e = run_err(d, "db.users.find({age: })").await;
    assert_eq!(
        e,
        "statement 1: find(): syntax error: expected value near `}` in `{age: }`"
    );
    let e = run_err(d, "db.things.insertOne({z: 1})\ndb.things.find({a: )").await;
    assert!(e.starts_with("statement 2:"), "{e}");
    assert_eq!(
        run(d, "db.things.countDocuments({z: 1})").await[0].rows,
        vec![vec![Value::Int(0)]]
    );
    let e = run_err(d, "db.users.countDocuments()\ndb.users.find({$foo: 1})").await;
    assert!(e.starts_with("statement 2:") && e.contains("$foo"), "{e}");
    let e = run_err(
        d,
        "db.users.insertOne({_id: ObjectId('650000000000000000000001')})",
    )
    .await;
    assert!(e.contains("duplicate key"), "{e}");
}

async fn typed_edits(d: &Arc<dyn Driver>) {
    run(
        d,
        "db.typed.insertMany([
           {_id: ObjectId('660000000000000000000001'), s: 'text', i: 5, i2: 6, f: 1.5, f2: 2.5, b: true,
            l: NumberLong(7), l2: NumberLong(8), dec: NumberDecimal('1.10'),
            o: ObjectId('660000000000000000000099'), o2: ObjectId('660000000000000000000099'),
            t: ISODate('2024-01-01T00:00:00Z'), t2: ISODate('2024-01-01T00:00:00Z'), t3: ISODate('2024-01-01T00:00:00Z'),
            doc: {a: 1}, arr: [1, 2], n: null, n2: null, gone: 'x'},
           {_id: ObjectId('660000000000000000000002'), m: 1, m2: 1},
         ])",
    )
    .await;
    let page = d.browse(&req("typed", "", None, 50, 0)).await.unwrap();
    let row = &page.result.rows[0];
    let original = |c: &str| row[page.result.columns.iter().position(|x| x == c).unwrap()].clone();
    assert_eq!(original("m"), Value::Missing);

    // (column, input, expected BSON): one row per line of the SPEC table.
    let oid98 = ObjectId::parse_str("660000000000000000000098").unwrap();
    let cases: Vec<(&str, Edit, Bson)> = vec![
        (
            "s",
            Edit::Text("new text".into()),
            Bson::String("new text".into()),
        ),
        ("i", Edit::Text("42".into()), Bson::Int32(42)),
        (
            "i2",
            Edit::Text("3000000000".into()),
            Bson::Int64(3_000_000_000),
        ),
        ("f", Edit::Text("2.25".into()), Bson::Double(2.25)),
        ("f2", Edit::Text("3".into()), Bson::Double(3.0)),
        ("b", Edit::Text("false".into()), Bson::Boolean(false)),
        ("l", Edit::Text("9".into()), Bson::Int64(9)),
        ("l2", Edit::Text("NumberLong(10)".into()), Bson::Int64(10)),
        (
            "dec",
            Edit::Text("2.50".into()),
            Bson::Decimal128("2.50".parse().unwrap()),
        ),
        (
            "o",
            Edit::Text("660000000000000000000098".into()),
            Bson::ObjectId(oid98),
        ),
        (
            "o2",
            Edit::Text("ObjectId(\"660000000000000000000098\")".into()),
            Bson::ObjectId(oid98),
        ),
        (
            "t",
            Edit::Text("2025-02-03T04:05:06Z".into()),
            Bson::DateTime(DateTime::from_millis(1_738_555_506_000)),
        ),
        (
            "t2",
            Edit::Text("ISODate(\"2025-02-03T04:05:06.007Z\")".into()),
            Bson::DateTime(DateTime::from_millis(1_738_555_506_007)),
        ),
        (
            "t3",
            Edit::Text("1000".into()),
            Bson::DateTime(DateTime::from_millis(1000)),
        ),
        (
            "doc",
            Edit::Text("{b: 2, a: NumberLong(1)}".into()),
            Bson::Document(doc! {"b": 2, "a": 1_i64}),
        ),
        (
            "arr",
            Edit::Text("[3, 'x']".into()),
            Bson::Array(vec![Bson::Int32(3), Bson::String("x".into())]),
        ),
        (
            "n",
            Edit::Text("{x: 1}".into()),
            Bson::Document(doc! {"x": 1}),
        ),
        (
            "n2",
            Edit::Text("plain text".into()),
            Bson::String("plain text".into()),
        ),
        ("m", Edit::Text("12".into()), Bson::Int32(12)),
        ("gone", Edit::Null, Bson::Null),
    ];
    let set: Vec<(String, Value, Edit)> = cases
        .iter()
        .map(|(c, e, _)| (c.to_string(), original(c), e.clone()))
        .collect();
    let key = vec![("_id".to_string(), original("_id"))];
    let changes = vec![Change::Update {
        key: key.clone(),
        set,
    }];
    let preview = d.preview(&obj("typed"), &changes).unwrap();
    eprintln!("typed preview: {}", preview[0]);
    assert_eq!(preview.len(), 1);
    assert!(preview[0].starts_with("db.typed.updateOne({_id: ObjectId('660000000000000000000001')}, {$set: {s: 'new text', i: 42, i2: NumberLong(3000000000), f: 2.25, f2: 3.0,"), "{}", preview[0]);
    assert_eq!(d.apply(&obj("typed"), &changes).await.unwrap(), 1);
    let stored = raw_doc("typed", "660000000000000000000001").await;
    for (c, _, want) in &cases {
        assert_eq!(stored.get(c), Some(want), "field {c}");
    }
    let doc_keys: Vec<&String> = stored.get_document("doc").unwrap().keys().collect();
    assert_eq!(doc_keys, ["b", "a"], "key order of an edited document");

    // Bad input: a clear error and no write.
    let bad = vec![Change::Update {
        key,
        set: vec![("i".into(), Value::Int(42), Edit::Text("abc".into()))],
    }];
    let e = d.apply(&obj("typed"), &bad).await.unwrap_err().to_string();
    eprintln!("bad typed edit: {e}");
    assert_eq!(e, "change 1: i: expected an integer, got `abc`");
    assert_eq!(
        raw_doc("typed", "660000000000000000000001").await.get("i"),
        Some(&Bson::Int32(42))
    );
}

async fn documents(d: &Arc<dyn Driver>) {
    let coll = obj("docs");
    let insert = Change::InsertDoc {
        json: "{_id: ObjectId('670000000000000000000001'), name: 'New', n: NumberLong(3), at: ISODate('2024-05-01T00:00:00Z'), keep: true}".into(),
    };
    let grid_insert = Change::Insert {
        values: vec![
            ("_id".into(), Edit::Text(String::new())),
            ("name".into(), Edit::Text("grid".into())),
            ("age".into(), Edit::Text("7".into())),
        ],
    };
    let preview = d
        .preview(&coll, &[insert.clone(), grid_insert.clone()])
        .unwrap();
    assert_eq!(
        preview,
        [
            "db.docs.insertOne({_id: ObjectId('670000000000000000000001'), name: 'New', n: NumberLong(3), at: ISODate('2024-05-01T00:00:00.000Z'), keep: true})",
            "db.docs.insertOne({name: 'grid', age: 7})",
        ]
    );
    assert_eq!(d.apply(&coll, &[insert, grid_insert]).await.unwrap(), 2);
    let stored = raw_doc("docs", "670000000000000000000001").await;
    assert_eq!(stored.get("n"), Some(&Bson::Int64(3)));
    assert_eq!(
        stored.get("at"),
        Some(&Bson::DateTime(DateTime::from_millis(1_714_521_600_000)))
    );
    let grid = raw()
        .await
        .collection::<Document>("docs")
        .find_one(doc! {"name": "grid"})
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(grid.get("_id"), Some(Bson::ObjectId(_))));
    assert_eq!(grid.get("age"), Some(&Bson::Int32(7)));

    // ReplaceDoc keeps the _id and drops the fields that are not in the new text.
    let key = vec![("_id".to_string(), oid_value("670000000000000000000001"))];
    let replace = Change::ReplaceDoc {
        key: key.clone(),
        json: "{name: 'Replaced', z: 1}".into(),
    };
    assert_eq!(
        d.preview(&coll, std::slice::from_ref(&replace)).unwrap(),
        [
            "db.docs.replaceOne({_id: ObjectId('670000000000000000000001')}, {name: 'Replaced', z: 1})"
        ]
    );
    assert_eq!(d.apply(&coll, &[replace]).await.unwrap(), 1);
    let stored = raw_doc("docs", "670000000000000000000001").await;
    assert_eq!(
        stored,
        doc! {"_id": ObjectId::parse_str("670000000000000000000001").unwrap(), "name": "Replaced", "z": 1}
    );
    let moved = Change::ReplaceDoc {
        key: key.clone(),
        json: "{_id: ObjectId('670000000000000000000009'), name: 'x'}".into(),
    };
    let e = d.apply(&coll, &[moved]).await.unwrap_err().to_string();
    assert!(e.contains("the _id cannot change"), "{e}");

    // Delete.
    assert_eq!(
        d.preview(&coll, &[Change::Delete { key: key.clone() }])
            .unwrap(),
        ["db.docs.deleteOne({_id: ObjectId('670000000000000000000001')})"]
    );
    assert_eq!(d.apply(&coll, &[Change::Delete { key }]).await.unwrap(), 1);
    assert_eq!(d.count(&req("docs", "", None, 50, 0)).await.unwrap(), 1);
}

async fn exactly_one(d: &Arc<dyn Driver>) {
    let coll = obj("users");
    let missing = vec![("_id".to_string(), oid_value("650000000000000000000999"))];
    let ann = vec![("_id".to_string(), oid_value("650000000000000000000001"))];
    let bob = vec![("_id".to_string(), oid_value("650000000000000000000002"))];
    let set_age = |key: &Vec<(String, Value)>, age: &str| Change::Update {
        key: key.clone(),
        set: vec![("age".into(), Value::Int(0), Edit::Text(age.into()))],
    };

    let e = d
        .apply(&coll, &[set_age(&missing, "1")])
        .await
        .unwrap_err()
        .to_string();
    eprintln!("update of a missing row: {e}");
    assert!(
        e.starts_with("change 1 of 1 failed: matched 0 documents, expected exactly 1"),
        "{e}"
    );
    let e = d
        .apply(
            &coll,
            &[Change::Delete {
                key: missing.clone(),
            }],
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("deleted 0 documents, expected exactly 1"), "{e}");

    // In order: stop at the first error. Change 1 stays, change 3 does not run.
    let changes = vec![
        set_age(&ann, "50"),
        Change::Delete { key: missing },
        set_age(&bob, "60"),
    ];
    let e = d.apply(&coll, &changes).await.unwrap_err().to_string();
    eprintln!("stop at first error: {e}");
    assert_eq!(
        e,
        "change 2 of 3 failed: deleted 0 documents, expected exactly 1. Change 1 was applied.\n\
         db.users.deleteOne({_id: ObjectId('650000000000000000000999')})"
    );
    assert_eq!(
        raw_doc("users", "650000000000000000000001")
            .await
            .get("age"),
        Some(&Bson::Int32(50))
    );
    assert_eq!(
        raw_doc("users", "650000000000000000000002")
            .await
            .get("age"),
        Some(&Bson::Int32(25))
    );
}

async fn read_only() {
    let cfg = ConnConfig {
        read_only: true,
        ..config(Kind::MongoDb)
    };
    let d = ourdb_mongo::connect(&cfg).await.unwrap();
    let p = d.browse(&req("users", "", None, 50, 0)).await.unwrap();
    assert!(p.key.is_empty());
    assert_eq!(p.read_only_reason.as_deref(), Some("read-only connection"));
    assert_eq!(
        run(&d, "db.users.countDocuments(); db.runCommand({ping: 1})")
            .await
            .len(),
        2
    );
    for text in [
        "db.users.insertOne({a: 1})",
        "db.users.find()\ndb.users.deleteMany({})",
        "db.users.aggregate([{$match: {}}, {$out: 'copy'}])",
        "db.runCommand({drop: 'users'})",
    ] {
        let e = run_err(&d, text).await;
        assert!(e.ends_with("read-only connection"), "{text}: {e}");
    }
    let key = vec![("_id".to_string(), oid_value("650000000000000000000001"))];
    let e = d
        .apply(&obj("users"), &[Change::Delete { key }])
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(e, "read-only connection");
    assert_eq!(d.count(&req("users", "", None, 50, 0)).await.unwrap(), 5);
}

async fn document_db() {
    // DocumentDB mode (retryWrites=false) works against the same server.
    let d = ourdb_mongo::connect(&config(Kind::DocumentDb))
        .await
        .unwrap();
    assert_eq!(d.kind(), Kind::DocumentDb);
    let rs = run(&d, "db.docdb.insertOne({_id: 1, a: 'x'})").await;
    assert_eq!(rs[0].affected, Some(1));
    let p = d
        .browse(&req("docdb", "{a: 'x'}", None, 50, 0))
        .await
        .unwrap();
    assert_eq!(
        p.result.rows,
        vec![vec![Value::Int(1), Value::Text("x".into())]]
    );
    let key = vec![("_id".to_string(), Value::Int(1))];
    let change = Change::Update {
        key,
        set: vec![("a".into(), Value::Text("x".into()), Edit::Text("y".into()))],
    };
    assert_eq!(d.apply(&obj("docdb"), &[change]).await.unwrap(), 1);
    d.close().await;
}

async fn url_and_login() {
    // URL without a password: the password comes from the config, and the
    // credential source is "admin" although the URL names it_mongo.
    let cfg = ConnConfig {
        url: format!("mongodb://ourdb@127.0.0.1:57017/{DB}"),
        host: "wrong".into(),
        ..config(Kind::MongoDb)
    };
    let d = ourdb_mongo::connect(&cfg).await.unwrap();
    assert_eq!(d.default_namespace().as_deref(), Some(DB));
    assert_eq!(d.count(&req("users", "", None, 50, 0)).await.unwrap(), 5);

    // Through a tunnel, the driver talks to the local port, not the URL host.
    let tunnel = ConnConfig {
        url: format!("mongodb://ourdb@db.internal.example:27017/{DB}"),
        via_tunnel: true,
        ..config(Kind::DocumentDb)
    };
    let d = ourdb_mongo::connect(&tunnel).await.unwrap();
    assert_eq!(d.count(&req("users", "", None, 50, 0)).await.unwrap(), 5);

    let bad = ConnConfig {
        password: "wrong".into(),
        ..config(Kind::MongoDb)
    };
    let e = ourdb_mongo::connect(&bad)
        .await
        .err()
        .expect("bad password must fail")
        .to_string();
    eprintln!("bad password: {e}");
    assert!(e.starts_with("authentication failed"), "{e}");
}
