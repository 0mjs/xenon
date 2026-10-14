#!/usr/bin/env python3
"""Seed data for Xenon's test fleet: the same shop, written for every engine.

    python3 dev/seed/generate.py [--size small|large]

Writes out/postgres.sql, out/mysql.sql (MySQL and MariaDB) and out/shop.db (SQLite).
Deterministic: the same size always gives the same rows.

Besides an ordinary shop (customers, products, orders, order items, events with JSON),
each engine gets a "type_zoo" table holding one of every type it has, with edge values,
and a table whose name and columns need quoting.
"""

import argparse
import json
import random
import sqlite3
from datetime import datetime, timedelta
from pathlib import Path

SIZES = {
    "small": dict(customers=500, products=120, orders=3_000, items_per_order=3, events=5_000),
    "large": dict(customers=50_000, products=2_000, orders=1_000_000, items_per_order=3, events=2_000_000),
}

FIRST = ["Aoife", "Liam", "Priya", "Tom", "Mei", "Ana", "Jonas", "Zoë", "Mateo", "Saanvi", "Kwame", "Olga",
         "Yuki", "Fatima", "Lars", "Chloé", "Diego", "Amara", "Noah", "Ines"]
LAST = ["Byrne", "Walsh", "Nair", "Okafor", "Chen", "Silva", "Berg", "Müller", "García", "Iyer", "Mensah",
        "Ivanova", "Tanaka", "Haddad", "Nilsen", "Dubois", "López", "Eze", "Smith", "Costa"]
CITIES = ["Dublin", "London", "Lisbon", "Berlin", "Tokyo", "Lagos", "São Paulo", "Mumbai", "Toronto", "Oslo"]
PLANS = ["free", "pro", "team", "enterprise"]
STATUSES = ["pending", "paid", "paid", "paid", "shipped", "refunded", "cancelled"]
EVENTS = ["page_view", "signup", "login", "add_to_cart", "checkout", "search", "logout"]
ADJ = ["Steel", "Quiet", "Rapid", "Linen", "Cedar", "Copper", "Nimbus", "Atlas", "Ember", "Fjord"]
NOUN = ["Kettle", "Lamp", "Backpack", "Notebook", "Chair", "Mug", "Speaker", "Desk", "Jacket", "Bottle"]

START = datetime(2024, 1, 1)


def when(rng, days=900):
    return START + timedelta(seconds=rng.randrange(days * 86_400))


def build(size):
    rng = random.Random(1729)
    n = SIZES[size]
    customers = []
    for i in range(1, n["customers"] + 1):
        first, last = rng.choice(FIRST), rng.choice(LAST)
        customers.append(dict(
            id=i,
            email=f"{first.lower()}.{last.lower()}.{i}@example.com".replace("ë", "e").replace("é", "e").replace("ü", "u").replace("í", "i").replace("ó", "o"),
            name=None if i % 17 == 0 else f"{first} {last}",
            city=rng.choice(CITIES),
            plan=rng.choice(PLANS),
            active=i % 9 != 0,
            created_at=when(rng),
        ))
    products = []
    for i in range(1, n["products"] + 1):
        products.append(dict(
            id=i,
            sku=f"SKU-{i:05d}",
            name=f"{rng.choice(ADJ)} {rng.choice(NOUN)}",
            price=round(rng.uniform(3, 400), 2),
            stock=rng.randrange(0, 500),
        ))
    orders, items = [], []
    item_id = 0
    for i in range(1, n["orders"] + 1):
        lines = rng.randint(1, n["items_per_order"] * 2 - 1)
        total = 0
        for _ in range(lines):
            item_id += 1
            product = rng.choice(products)
            qty = rng.randint(1, 4)
            total += product["price"] * qty
            items.append(dict(id=item_id, order_id=i, product_id=product["id"], quantity=qty,
                              unit_price=product["price"]))
        orders.append(dict(
            id=i,
            customer_id=rng.randrange(1, n["customers"] + 1),
            status=rng.choice(STATUSES),
            total=round(total, 2),
            note=None if rng.random() < 0.85 else rng.choice(["Gift wrap please", "Leave at the door",
                                                             "Call on arrival", "Fragile: glass"]),
            created_at=when(rng),
        ))
    events = []
    for i in range(1, n["events"] + 1):
        name = rng.choice(EVENTS)
        props = {"path": rng.choice(["/", "/pricing", "/docs", "/cart", "/account"]),
                 "ms": rng.randrange(20, 4_000)}
        if name == "search":
            props["query"] = rng.choice(["kettle", "steel lamp", "café", "日本"])
        events.append(dict(id=i, customer_id=None if rng.random() < 0.2 else rng.randrange(1, n["customers"] + 1),
                           name=name, props=json.dumps(props, ensure_ascii=False), at=when(rng)))
    return dict(customers=customers, products=products, orders=orders, order_items=items, events=events)


# ---------------------------------------------------------------- dialects

def lit(value, mysql):
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return ("1" if value else "0") if mysql else ("true" if value else "false")
    if isinstance(value, (int, float)):
        return repr(value)
    if isinstance(value, datetime):
        return f"'{value:%Y-%m-%d %H:%M:%S}'"
    text = str(value).replace("'", "''")
    if mysql:
        text = text.replace("\\", "\\\\")
    return f"'{text}'"


def inserts(table, rows, mysql, quote, batch=500):
    if not rows:
        return ""
    cols = list(rows[0])
    head = f"insert into {quote(table)} ({', '.join(quote(c) for c in cols)}) values\n"
    out = []
    for start in range(0, len(rows), batch):
        chunk = rows[start:start + batch]
        values = ",\n".join("(" + ", ".join(lit(r[c], mysql) for c in cols) + ")" for r in chunk)
        out.append(head + values + ";\n")
    return "".join(out)


POSTGRES_SCHEMA = """
create type order_status as enum ('pending', 'paid', 'shipped', 'refunded', 'cancelled');
create table customers (
  id integer primary key, email text not null unique, name text, city text, plan text not null,
  active boolean not null default true, created_at timestamptz not null);
create table products (
  id integer primary key, sku text not null unique, name text not null, price numeric(10,2) not null,
  stock integer not null);
create table orders (
  id integer primary key, customer_id integer not null references customers(id), status order_status not null,
  total numeric(12,2) not null, note text, created_at timestamptz not null);
create table order_items (
  id integer primary key, order_id integer not null references orders(id), product_id integer not null references products(id),
  quantity integer not null, unit_price numeric(10,2) not null);
create table events (
  id bigint primary key, customer_id integer references customers(id), name text not null, props jsonb not null, at timestamptz not null);
create index orders_by_customer on orders (customer_id);
create index events_by_name_at on events (name, at);
create view paid_orders as select * from orders where status = 'paid';
create schema analytics;
create materialized view analytics.daily_revenue as
  select date_trunc('day', created_at) as day, count(*) as orders, sum(total) as revenue from orders group by 1;

create table type_zoo (
  id serial primary key, small smallint, big bigint, exact numeric(30,10), money_col money, real_col real, dbl double precision,
  flag boolean, label varchar(40), body text, fixed char(5), born date, at_time time, at_tz timestamptz, at_naive timestamp,
  span interval, uid uuid, ip inet, mac macaddr, bits bit varying(16), tags text[], matrix integer[][], doc jsonb, raw json,
  bytes bytea, point_col point, range_col int4range, xml_col xml, status order_status);
insert into type_zoo (small, big, exact, money_col, real_col, dbl, flag, label, body, fixed, born, at_time, at_tz, at_naive,
  span, uid, ip, mac, bits, tags, matrix, doc, raw, bytes, point_col, range_col, xml_col, status) values
  (32767, 9223372036854775807, 12345678901234567890.0123456789, 1234.56, 3.14159, 2.718281828459045, true, 'max values',
   'A long body. ' || repeat('Lorem ipsum dolor sit amet. ', 40), 'abc', '2024-02-29', '23:59:59', '2024-12-31 23:59:59+05:30',
   '1999-12-31 23:59:59', '1 year 2 mons 3 days 04:05:06', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '192.168.0.1/24',
   '08:00:2b:01:02:03', B'1010101', '{"red","green","with space","with,comma"}', '{{1,2},{3,4}}',
   '{"nested": {"deep": [1, 2, {"x": null}]}, "emoji": "🦀"}', '{"raw": true}', '\\xdeadbeef', '(1.5,-2)', '[1,10)',
   '<a href="x">xml</a>', 'paid'),
  (-32768, -9223372036854775808, -0.0000000001, -0.01, 'NaN', '-Infinity', false, '', '', '', '0001-01-01', '00:00:00',
   '1970-01-01 00:00:00+00', '1970-01-01 00:00:00', '-1 day', '00000000-0000-0000-0000-000000000000', '::1', '00:00:00:00:00:00',
   B'', '{}', '{}', '[]', 'null', '\\x', '(0,0)', 'empty', '<e/>', 'pending'),
  (null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null,
   null, null, null, null, null, null, null, null);

create table "Order Items (archive)" ("order" integer, "select" text, "Mixed Case" text, "naïve ✓" text, "emoji 🦀" text);
insert into "Order Items (archive)" values (1, 'reserved words', 'Mixed', 'ünïcödé', '🦀🐘🐬'), (2, null, '', 'tab\there', 'line
break');
"""

MYSQL_SCHEMA = """
set names utf8mb4;
create table customers (
  id int primary key, email varchar(255) not null unique, name varchar(255), city varchar(100), plan varchar(20) not null,
  active tinyint(1) not null default 1, created_at datetime not null);
create table products (
  id int primary key, sku varchar(20) not null unique, name varchar(255) not null, price decimal(10,2) not null, stock int not null);
create table orders (
  id int primary key, customer_id int not null, status enum('pending','paid','shipped','refunded','cancelled') not null,
  total decimal(12,2) not null, note text, created_at datetime not null,
  key orders_by_customer (customer_id), foreign key (customer_id) references customers(id));
create table order_items (
  id int primary key, order_id int not null, product_id int not null, quantity int not null, unit_price decimal(10,2) not null,
  foreign key (order_id) references orders(id), foreign key (product_id) references products(id));
create table events (
  id bigint primary key, customer_id int, name varchar(40) not null, props json not null, at datetime not null,
  key events_by_name_at (name, at));
create view paid_orders as select * from orders where status = 'paid';

create table type_zoo (
  id int auto_increment primary key, tiny tinyint, small smallint, medium mediumint, big bigint, ubig bigint unsigned,
  exact decimal(30,10), flt float, dbl double, flag boolean, bits bit(8), label varchar(40), fixed char(5), body longtext,
  born date, at_time time(3), at_dt datetime(6), at_ts timestamp null, yr year, uid binary(16), bytes blob, doc json,
  status enum('pending','paid'), perms set('read','write','admin'), geo point);
insert into type_zoo (tiny, small, medium, big, ubig, exact, flt, dbl, flag, bits, label, fixed, body, born, at_time, at_dt, at_ts,
  yr, uid, bytes, doc, status, perms, geo) values
  (127, 32767, 8388607, 9223372036854775807, 18446744073709551615, 12345678901234567890.0123456789, 3.14159, 2.718281828459045,
   true, b'10101010', 'max values', 'abc', concat('A long body. ', repeat('Lorem ipsum dolor sit amet. ', 40)), '2024-02-29',
   '838:59:59', '2024-12-31 23:59:59.999999', '2038-01-19 03:14:07', 2155, unhex('a0eebc999c0b4ef8bb6d6bb9bd380a11'),
   unhex('deadbeef'), '{"nested": {"deep": [1, 2, {"x": null}]}, "emoji": "🦀"}', 'paid', 'read,admin', ST_GeomFromText('POINT(1.5 -2)')),
  (-128, -32768, -8388608, -9223372036854775808, 0, -0.0000000001, -1e-30, -1.7976931348623157e308, false, b'0', '', '', '',
   '1000-01-01', '-838:59:59', '1000-01-01 00:00:00', '1970-01-01 00:00:01', 1901, unhex('00000000000000000000000000000000'),
   '', '[]', 'pending', '', ST_GeomFromText('POINT(0 0)')),
  (null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null, null,
   null, null, null);

-- MySQL and MariaDB identifiers can't hold 4-byte characters (emoji), so this column differs.
create table `Order Items (archive)` (`order` int, `select` text, `Mixed Case` text, `naïve ✓` text, `symbols ★✓` text);
insert into `Order Items (archive)` values (1, 'reserved words', 'Mixed', 'ünïcödé', '🦀🐘🐬'), (2, null, '', 'tab\\there', 'line\\nbreak');
"""

SQLITE_SCHEMA = """
create table customers (id integer primary key, email text not null unique, name text, city text, plan text not null,
  active integer not null default 1, created_at text not null);
create table products (id integer primary key, sku text not null unique, name text not null, price numeric not null, stock integer not null);
create table orders (id integer primary key, customer_id integer not null references customers(id), status text not null,
  total numeric not null, note text, created_at text not null);
create table order_items (id integer primary key, order_id integer not null references orders(id),
  product_id integer not null references products(id), quantity integer not null, unit_price numeric not null);
create table events (id integer primary key, customer_id integer references customers(id), name text not null, props text not null, at text not null);
create index orders_by_customer on orders (customer_id);
create view paid_orders as select * from orders where status = 'paid';
create table type_zoo (id integer primary key, i integer, r real, n numeric, t text, b blob, anything);
insert into type_zoo (i, r, n, t, b, anything) values
  (9223372036854775807, 1.7976931348623157e308, 12345678901234567890.0123456789, 'ünïcödé 🦀', x'deadbeef', 'text in an untyped column'),
  (-9223372036854775808, -0.0, 0, '', x'', 42),
  (null, null, null, null, null, null);
create table "Order Items (archive)" ("order" integer, "select" text, "Mixed Case" text, "naïve ✓" text, "emoji 🦀" text);
insert into "Order Items (archive)" values (1, 'reserved words', 'Mixed', 'ünïcödé', '🦀🐘🐬');
"""


def write_sql(path, schema, data, mysql):
    quote = (lambda n: f"`{n}`") if mysql else (lambda n: f'"{n}"')
    with open(path, "w", encoding="utf-8") as f:
        f.write("-- Generated by dev/seed/generate.py. Do not edit.\n")
        f.write(schema)
        if not mysql:
            f.write("begin;\n")
        for table in ["customers", "products", "orders", "order_items", "events"]:
            f.write(inserts(table, data[table], mysql, quote))
        if not mysql:
            f.write("commit;\nrefresh materialized view analytics.daily_revenue;\nanalyze;\n")


def write_sqlite(path, data):
    path.unlink(missing_ok=True)
    db = sqlite3.connect(path)
    db.executescript(SQLITE_SCHEMA)
    for table in ["customers", "products", "orders", "order_items", "events"]:
        rows = data[table]
        cols = list(rows[0])
        db.executemany(
            f"insert into {table} ({', '.join(cols)}) values ({', '.join('?' for _ in cols)})",
            [[(f"{v:%Y-%m-%d %H:%M:%S}" if isinstance(v, datetime) else v) for v in (r[c] for c in cols)] for r in rows],
        )
    db.commit()
    db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--size", choices=SIZES, default="small")
    args = parser.parse_args()
    out = Path(__file__).resolve().parent / "out"
    out.mkdir(exist_ok=True)
    stamp = out / ".size"
    if stamp.exists() and stamp.read_text() == args.size and (out / "postgres.sql").exists():
        return
    data = build(args.size)
    write_sql(out / "postgres.sql", POSTGRES_SCHEMA, data, mysql=False)
    write_sql(out / "mysql.sql", MYSQL_SCHEMA, data, mysql=True)
    write_sqlite(out / "shop.db", data)
    stamp.write_text(args.size)
    rows = sum(len(v) for v in data.values())
    print(f"seed: {args.size}, {rows:,} rows per engine -> {out}")


if __name__ == "__main__":
    main()
