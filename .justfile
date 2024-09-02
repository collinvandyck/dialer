# runs the checker, restarting when changes are detected.
dev:
    watchexec \
        -v \
        --log-file we.log \
        -c reset --restart --wrap-process session --stop-timeout 1s cargo run
