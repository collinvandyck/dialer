#!/usr/bin/env bash

# vim: set noexpandtab:
# shellcheck disable=all

batsql() {
	bat -f -l sql --theme base16 -P "$@"
}

if [[ "ok" ]];then
	printf "raw results data:\n\n"
	batsql <(
		sqlite3 checks.db <<-eos
			SELECT
				r.check_id,
				c.name,
				c.kind,
				r.epoch,
				r.ms,
				r.err
			FROM results r
			JOIN checks c on r.check_id = c.id
			LIMIT 10
			eos
		)
	printf "\n"
fi


if [[ "ok" ]];then
	printf "rolled up results:\n\n"
	batsql <(
		sqlite3 checks.db <<-eos
			.parameter set @rollup 5
			.parameter set @secs 30
			SELECT
				r.check_id,
				c.name,
				c.kind,
				r.epoch / @rollup * @rollup AS bucket,
				time(r.epoch / @rollup * @rollup, 'unixepoch') as time,
				CAST(AVG(r.ms) as INTEGER) AS avg_ms,
				COUNT(r.err) AS errors,
				COUNT(r.ms) AS measures
			FROM results r
			JOIN checks c on r.check_id = c.id
			WHERE r.epoch >= strftime('%s', 'now') - @secs
			GROUP BY r.check_id, c.name, c.kind, bucket
			eos
		)
fi

