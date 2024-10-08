queries:
    viddy -n 1 scripts/rollup.sh

loop:
    bacon loop

db:
    sqlite3 checks.db

clean:
    rm -f checks.db*

watch:
    watchexec \
        -v \
        --restart \
        just clean dev

image-old:
    docker build -t dialer .

image:
    docker --debug build -f Dockerfile.ubuntu -t dialer .

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

