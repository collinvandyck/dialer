#!/usr/bin/env bash

if [[ "$1" == "--clean" ]]; then
    #rm -f checks.db*
    shift
fi

exec cargo run  --color always "$@"

