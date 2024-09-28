loop:
    watchexec -r -- sh -c "'cargo run'"

db:
    sqlite3 checks.db

clean:
    rm -f checks.db

watch:
    watchexec \
        -v \
        --restart \
        just clean dev

tail_http:
    #!/usr/bin/env bash
    watch --interval 0.5 "sqlite3 checks.db 'SELECT *
        FROM (
            SELECT *
            FROM http_resp
            ORDER BY id DESC
            LIMIT 10
        ) subquery
        ORDER BY id ASC;'"

