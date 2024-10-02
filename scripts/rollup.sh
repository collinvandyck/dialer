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
	printf "using:\n\n"
	batsql <(
		sqlite3 checks.db <<-eos
		    with params as (
				select
					10 as rollup,
					60 as secs
			)
			SELECT
				r.check_id,
				c.name,
				c.kind,
				r.epoch / p.rollup * p.rollup AS bucket,
				datetime(r.epoch / p.rollup * p.rollup, 'unixepoch') as time,
				CAST(MIN(r.ms) as INTEGER) AS min,
				CAST(AVG(r.ms) as INTEGER) AS avg,
				CAST(MAX(r.ms) as INTEGER) AS max,
				COUNT(*) AS count,
				COUNT(r.err) AS errs
			FROM results r
			JOIN params AS p
			JOIN checks c on r.check_id = c.id
			WHERE bucket >= (strftime('%s', 'now')) - p.secs
			GROUP BY r.check_id, c.name, c.kind, bucket
			ORDER BY bucket, name, kind
			eos
		)
fi

