#!/usr/bin/env bash

#rm -f checks.db*
exec cargo run  --color always "$@"
