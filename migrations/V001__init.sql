create table checks (
    id integer primary key autoincrement,
    name text not null,
    kind text not null check(kind in ('http', 'ping'))
);

create table results (
    id integer primary key autoincrement,
    check_id integer not null,
    FOREIGN KEY(check_id) REFERENCES checks(id)
);

create table http_resp (
    id integer primary key autoincrement,
    check_name text not null,
    ts timestamp default current_timestamp,
    latency_ms integer not null,
    code integer,
    error text,
    error_kind text
);
create index idx_http_resp_ts on http_resp(ts);

create table ping_resp (
    id integer primary key autoincrement,
    check_name text not null,
    ts timestamp default current_timestamp,
    latency_ms integer not null,
    error text,
    error_kind text
);
create index idx_ping_resp_ts on ping_resp(ts);
