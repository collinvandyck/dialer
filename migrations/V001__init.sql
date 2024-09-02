create table http_resp (
    id integer primary key autoincrement,
    check_name text not null,
    ts timestamp default current_timestamp,
    latency_ms integer not null,
    code integer,
    error text
);
create index idx_http_resp_ts on http_resp(ts);
