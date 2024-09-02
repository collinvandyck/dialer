dev:
    cargo run

clean:
    rm -f checks.db

watch:
    watchexec \
        -v \
        --restart \
        just clean dev
