create table checks (
    id integer primary key autoincrement,
    name text not null,
    kind text not null check(kind in ('http', 'ping'))
);

create table results (
    id integer primary key autoincrement,
    check_id integer not null,
    epoch integer not null default (CAST(strftime('%s', 'now') AS INTEGER)),
    ms integer,
    err text,
    FOREIGN KEY(check_id) REFERENCES checks(id)
);
create index idx_results_epoch on results(epoch);
