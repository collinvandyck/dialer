#!/usr/bin/env bash

# vim: set noexpandtab:
# shellcheck disable=all

if [[ "" ]]; then
    echo ".schema" | sqlite3 checks.db
    echo ""
fi

if [[ "ok" ]]; then
	sqlite3 checks.db <<-EOF
		select count(*) from results;
	EOF
	echo ""
fi

if [[ "ok" ]];then
	sqlite3 checks.db <<-eos
		select * from results limit 10;
	eos
fi

