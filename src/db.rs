use crate::{
    api,
    check::{self},
};
use eyre::Context;
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::task::{self, JoinError};
use tracing::instrument;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Generic(#[from] rusqlite::Error),

    #[error("migrate: {0}")]
    Migrate(#[source] refinery::Error),

    #[error("db pool: {0}")]
    CreatePool(#[source] r2d2::Error),

    #[error("blocking thread panicked: {0}")]
    BlockingThreadPanic(#[source] JoinError),

    #[error("could not get conn: {0}")]
    GetConn(#[source] r2d2::Error),

    #[error("prepare stmt: {err}")]
    Prepare {
        #[source]
        err: rusqlite::Error,
    },
}

type DbPool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone, Debug)]
pub struct Db {
    pool: Arc<DbPool>,
}

mod record {
    // represents a record in the http_resp table
    #[derive(Debug)]
    pub struct Http {
        name: String,
        ts: String,
        latency_ms: i32,
        code: Option<u32>,
        error: Option<String>,
        error_kind: Option<String>,
    }

    #[derive(Debug)]
    pub struct Ping {
        name: String,
        ts: String,
        latency_ms: i32,
        error: Option<String>,
        error_kind: Option<String>,
    }

    #[derive(Debug)]
    pub struct Union {
        name: String,
        kind: String,
        ts: String,
        latency_ms: i32,
        error: Option<String>,
        error_kind: Option<String>,
    }

    impl<'conn> TryFrom<&rusqlite::Row<'conn>> for Http {
        type Error = rusqlite::Error;
        fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
            Ok(Self {
                name: row.get("check_name")?,
                ts: row.get("ts")?,
                latency_ms: row.get("latency_ms")?,
                code: row.get("code")?,
                error: row.get("error")?,
                error_kind: row.get("error_kind")?,
            })
        }
    }

    impl<'conn> TryFrom<&rusqlite::Row<'conn>> for Ping {
        type Error = rusqlite::Error;
        fn try_from(row: &rusqlite::Row<'conn>) -> Result<Self, Self::Error> {
            Ok(Self {
                name: row.get("check_name")?,
                ts: row.get("ts")?,
                latency_ms: row.get("latency_ms")?,
                error: row.get("error")?,
                error_kind: row.get("error_kind")?,
            })
        }
    }

    impl<'conn> TryFrom<&rusqlite::Row<'conn>> for Union {
        type Error = rusqlite::Error;
        fn try_from(row: &rusqlite::Row) -> Result<Self, Self::Error> {
            Ok(Self {
                name: row.get("check_name")?,
                kind: row.get("kind")?,
                ts: row.get("ts")?,
                latency_ms: row.get("latency_ms")?,
                error: row.get("error")?,
                error_kind: row.get("error_kind")?,
            })
        }
    }
}

// converts a blocking thread panic into an Error
fn handle_panic<T>(err: JoinError) -> Result<T, Error> {
    Err(Error::BlockingThreadPanic(err))
}

impl Db {
    pub async fn connect(path: &Path) -> Result<Self, Error> {
        let path = path.to_path_buf();
        task::spawn_blocking(move || {
            Self::migrate(&path)?;
            let mgr = SqliteConnectionManager::file(path);
            let pool = r2d2::Pool::new(mgr).map_err(Error::CreatePool)?;
            let pool = Arc::new(pool);
            Ok(Db { pool })
        })
        .await
        .unwrap_or_else(handle_panic)
    }

    pub async fn query(&self, query: api::Query) -> Result<api::Metrics, Error> {
        let db = self.clone();
        task::spawn_blocking(move || db.query_sync(query))
            .await
            .unwrap_or_else(handle_panic)
    }

    #[instrument(skip_all)]
    fn query_sync(&self, query: api::Query) -> Result<api::Metrics, Error> {
        tracing::info!("Querying for metrics data");
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare_cached(
                "select * from (
                    select
                    check_name, 'http' as kind, ts, latency_ms, error, error_kind from http_resp
                    union
                    select
                    check_name, 'ping' as kind, ts, latency_ms, error, error_kind from ping_resp
                )
                ",
            )
            .map_err(|err| Error::Prepare { err })?;
        let res = stmt.query_map([], |row| record::Union::try_from(row))?;
        let unions: Vec<record::Union> = res.collect::<Result<_, _>>()?;
        for rec in unions {
            tracing::info!("union: {rec:#?}");
        }
        Ok(api::Metrics::default())
    }

    // ensures that the check has a record in the db with an id. not used at the moment but i
    // wanted the pk to be available in case we wanted to use it elsewhere.
    pub async fn materialize(&self, name: &str, kind: check::Kind) -> Result<u64, Error> {
        let db = self.clone();
        let name = name.to_string();
        let kind = kind.as_str();
        task::spawn_blocking(move || {
            let conn = db.conn()?;
            let id = conn
                .query_row(
                    "select id from checks where name=?1 and kind=?2",
                    (&name, kind),
                    |row| {
                        let id: u64 = row.get(0)?;
                        Ok(id)
                    },
                )
                .optional()?;
            let id = match id {
                Some(id) => id,
                None => conn.query_row(
                    "insert into checks (name, kind) values (?1, ?2) returning id",
                    (&name, kind),
                    |row| {
                        let id: u64 = row.get(0)?;
                        Ok(id)
                    },
                )?,
            };
            Ok(id)
        })
        .await
        .unwrap_or_else(handle_panic)
    }

    pub async fn record_http(
        &self,
        check: &check::Http,
        res: Result<check::HttpResult, check::HttpError>,
    ) -> Result<(), Error> {
        let db = self.clone();
        match res {
            Ok(check::HttpResult { resp, latency }) => {
                let code = resp.status().as_u16();
                let latency = latency.as_millis() as i64;
                let name = check.name.clone();
                task::spawn_blocking(move || {
                    db.conn()?.execute(
                        "insert into http_resp
                        (check_name, latency_ms, code)
                        values (?1, ?2, ?3)",
                        (name, latency, code),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(handle_panic)?;
            }
            Err(err) => {
                let kind = match &err {
                    check::HttpError::BuildHttpClient(_) => "build_client",
                    check::HttpError::BuildHttpRequest(_) => "build_request",
                    check::HttpError::TaskTimeout(_) => "task_timeout",
                    check::HttpError::Error { err, latency } => "call",
                };
                let err = err.to_string();
                let name = check.name.clone();
                task::spawn_blocking(move || {
                    db.conn()?.execute(
                        "insert into http_resp
                        (check_name, error, error_kind)
                        values (?1, ?2, ?3)",
                        (name, err, kind),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(handle_panic)?;
            }
        };
        Ok(())
    }

    pub async fn record_ping(
        &self,
        check: &check::Ping,
        res: Result<check::PingResult, check::PingError>,
    ) -> Result<(), Error> {
        let db = self.clone();
        match res {
            Ok(check::PingResult { packet, latency }) => {
                let name = check.name.clone();
                let latency = latency.as_millis() as i64;
                task::spawn_blocking(move || {
                    db.conn()?.execute(
                        "insert into ping_resp (check_name, latency_ms) values (?1, ?2)",
                        (name, latency),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(handle_panic)?;
            }
            Err(err) => {
                let name = check.name.clone();
                let kind = match &err {
                    check::PingError::ResolveHost { .. } => "resolve_host",
                    check::PingError::NoIpForHost { .. } => "no_ip_for_host",
                    check::PingError::Ping { .. } => "ping",
                    check::PingError::TaskTimeout(_) => "task_timeout",
                };
                let err = err.to_string();
                task::spawn_blocking(move || {
                    db.conn()?.execute(
                        "insert into ping_resp (check_name, error, error_kind)",
                        (name, err, kind),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(handle_panic)?;
            }
        }
        Ok(())
    }

    /// fetch a new conn from the underlying pool
    fn conn(&self) -> Result<PooledConnection<SqliteConnectionManager>, Error> {
        self.pool.get().map_err(Error::GetConn)
    }

    /// migrates the db at the specified path. is not compatible with the sqlite pool so we open a
    /// connection manually.
    fn migrate(path: &Path) -> Result<(), Error> {
        let mut conn = Connection::open(path)?;
        migrate::migrations::runner()
            .run(&mut conn)
            .map_err(Error::Migrate)?;
        Ok(())
    }
}

mod migrate {
    refinery::embed_migrations!("./migrations");
}

#[cfg(test)]
mod tests {
    use super::*;
    use check::Kind;
    use rusqlite::Connection;

    #[test]
    fn migrations() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate::migrations::runner().run(&mut conn).unwrap();
    }

    #[tokio::test]
    async fn materialize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.as_ref().join("test.db");
        let db = Db::connect(&path).await.unwrap();
        let id = db.materialize("foo", Kind::Ping).await.unwrap();
        assert_eq!(id, 1);
        let id = db.materialize("foo", Kind::Ping).await.unwrap();
        assert_eq!(id, 1);
        let id = db.materialize("bar", Kind::Ping).await.unwrap();
        assert_eq!(id, 2);
        let id = db.materialize("baz", Kind::Http).await.unwrap();
        assert_eq!(id, 3);
        let id = db.materialize("baz", Kind::Http).await.unwrap();
        assert_eq!(id, 3);
    }
}
