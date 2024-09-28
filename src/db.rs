use crate::{
    api,
    check::{self},
};
use eyre::Context;
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension};
use std::{path::Path, sync::Arc};
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

    #[error("blocking task panicked: {0}")]
    JoinError(#[source] JoinError),

    #[error("could not get conn: {0}")]
    GetConn(#[source] r2d2::Error),
}

type DbPool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone, Debug)]
pub struct Db {
    pool: Arc<DbPool>,
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
        .unwrap_or_else(|err| Err(Error::JoinError(err)))
    }

    pub async fn query(&self, query: api::Query) -> Result<api::Metrics, Error> {
        let db = self.clone();
        // todo: any async logic before delegating to blocking threadpol.
        task::spawn_blocking(move || {
            // delegate to sync method.
            db.query_sync(query)
        })
        .await
        .unwrap_or_else(|err| Err(Error::JoinError(err)))
    }

    #[instrument(skip_all)]
    fn query_sync(&self, query: api::Query) -> Result<api::Metrics, Error> {
        tracing::info!("Querying for metrics data");
        let conn = self.conn()?;
        Ok(api::Metrics::default())
    }

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
        .unwrap_or_else(|err| Err(Error::JoinError(err)))
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
                .unwrap_or_else(|err| Err(Error::JoinError(err)))?;
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
                .unwrap_or_else(|err| Err(Error::JoinError(err)))?;
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
                .unwrap_or_else(|err| Err(Error::JoinError(err)))?;
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
                .unwrap_or_else(|err| Err(Error::JoinError(err)))?;
            }
        }
        Ok(())
    }

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
