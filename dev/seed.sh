#!/usr/bin/env bash
# Load a small "shop" sample into the dev databases (dev/compose.yml) and
# write dev/shop.sqlite. Safe to run again: it drops and recreates the sample.
set -euo pipefail
cd "$(dirname "$0")"

SQL_PG="
DROP SCHEMA IF EXISTS shop CASCADE;
CREATE SCHEMA shop;
CREATE TABLE shop.customers (
  id serial PRIMARY KEY,
  name text NOT NULL,
  email text,
  plan text DEFAULT 'free',
  credits numeric(10,2) DEFAULT 0,
  tags text[],
  profile jsonb,
  created_at timestamptz DEFAULT now()
);
INSERT INTO shop.customers (name, email, plan, credits, tags, profile, created_at)
SELECT 'Customer ' || g,
       CASE WHEN g % 7 = 0 THEN NULL WHEN g % 11 = 0 THEN '' ELSE 'c' || g || '@example.com' END,
       (ARRAY['free','pro','team'])[1 + g % 3],
       round((g * 3.7)::numeric, 2),
       ARRAY['t' || g % 4],
       jsonb_build_object('age', 20 + g % 50, 'city', (ARRAY['Hanoi','Tokyo','Osaka'])[1 + g % 3]),
       timestamptz '2026-01-01' + g * interval '7 hours'
FROM generate_series(1, 500) g;
CREATE VIEW shop.pro_customers AS SELECT * FROM shop.customers WHERE plan = 'pro';"

SQL_MY="
DROP DATABASE IF EXISTS shop;
CREATE DATABASE shop;
CREATE TABLE shop.orders (
  id int AUTO_INCREMENT PRIMARY KEY,
  customer varchar(100) NOT NULL,
  total decimal(10,2) NOT NULL,
  status enum('new','paid','shipped') DEFAULT 'new',
  note text,
  created_at datetime DEFAULT CURRENT_TIMESTAMP
);
INSERT INTO shop.orders (customer, total, status, note) VALUES
  ('Anna', 12.50, 'paid', NULL), ('Binh', 99.00, 'new', 'gift wrap'),
  ('Chen', 5.25, 'shipped', ''), ('Dung', 42.00, 'paid', 'O''Brien said \"hi\"'),
  ('Emi', 7.75, 'new', 'emoji 🍣');"

docker compose -f compose.yml exec -T postgres psql -q -U ourdb -d ourdb -v ON_ERROR_STOP=1 -c "$SQL_PG"
docker compose -f compose.yml exec -T mysql mysql -uroot -pourdb -e "$SQL_MY" 2>/dev/null
docker compose -f compose.yml exec -T mongo mongosh --quiet -u ourdb -p ourdb --authenticationDatabase admin shop --eval '
db.events.drop();
db.events.insertMany([
  {type: "login", user: "anna", at: new Date("2026-11-01T09:00:00Z"), meta: {ip: "10.0.0.1", ok: true}},
  {type: "order", user: "binh", at: new Date("2026-11-02T10:30:00Z"), amount: NumberDecimal("99.00"), items: [1, 2, 3]},
  {type: "logout", user: "anna", at: new Date("2026-11-03T18:00:00Z")},
  {type: "order", user: "chen", amount: NumberDecimal("5.25"), count: NumberLong(3), note: null}
]);' >/dev/null

rm -f shop.sqlite
python3 - <<'PY'
import sqlite3
c = sqlite3.connect("shop.sqlite")
c.executescript("""
CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT NOT NULL, price REAL, stock INTEGER DEFAULT 0, image BLOB);
INSERT INTO products (name, price, stock, image) VALUES
  ('Coffee', 3.5, 120, x'89504e47'), ('Tea', 2.0, 80, NULL), ('Cake', 4.25, 0, NULL);
CREATE VIEW in_stock AS SELECT * FROM products WHERE stock > 0;
""")
c.commit()
PY
echo "Seeded: postgres shop.customers, mysql shop.orders, mongo shop.events, dev/shop.sqlite"
