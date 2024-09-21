use crate::check::{self, Check};
use eyre::Context;
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension};
use std::{path::Path, sync::Arc};
use tokio::task::{self, JoinError};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbCheck<Check> {
    check: Check,
    id: u64,
}

impl<C: Check> DbCheck<C> {
    fn new(check: &C, id: u64) -> Self {
        Self {
            check: check.clone(),
            id,
        }
    }

    fn from_id_row(check: &C, row: &rusqlite::Row) -> Result<Self, Error> {
        let id = row.get(0)?;
        Ok(Self::new(check, id))
    }
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

    pub async fn ensure_check<C>(&self, check: &C) -> Result<DbCheck<C>, Error>
    where
        C: Check + 'static,
    {
        let db = self.clone();
        let check = check.clone();
        task::spawn_blocking(move || {
            let conn = db.pool.get().map_err(Error::GetConn)?;
            let name = check.name();
            let kind = check.kind().as_str();
            conn.query_row(
                "select id from checks where name=?1 and kind=?2",
                (&name, kind),
                |row| Ok(DbCheck::from_id_row(&check, row)),
            )
            .optional()?
            .unwrap_or_else(|| {
                conn.query_row(
                    "insert into checks (name, kind) values (?1, ?2) returning id",
                    (&name, kind),
                    |row| Ok(DbCheck::from_id_row(&check, row)),
                )?
            })
        })
        .await
        .unwrap_or_else(|err| Err(Error::JoinError(err)))
    }

    async fn record_http(&self, check: &check::Http, res: &check::HttpResult) -> Result<(), Error> {
        let db = self.clone();
        match res {
            check::HttpResult::Response { resp, latency } => {
                let name = check.name();
                let code = resp.status().as_u16();
                let latency = latency.as_millis() as i64;
                task::spawn_blocking(move || {
                    let conn = db.pool.get().map_err(Error::GetConn)?;
                    conn.execute(
                        "insert into http_resp
                        (check_name, latency_ms, code)
                        values (?1, ?2, ?3)",
                        (name, latency, code),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(|err| Err(Error::JoinError(err)))
            }
            check::HttpResult::Error { err, latency } => {
                let name = check.name();
                let kind = {
                    if err.is_status() {
                        "status"
                    } else if err.is_body() {
                        "body"
                    } else if err.is_decode() {
                        "decode"
                    } else {
                        "unknown"
                    }
                };
                let err = err.to_string();
                task::spawn_blocking(move || {
                    let conn = db.pool.get().map_err(Error::GetConn)?;
                    conn.execute(
                        "insert into http_resp
                        (check_name, error, error_kind)
                        values (?1, ?2, ?3)",
                        (name, err, kind),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(|err| Err(Error::JoinError(err)))
            }
        }
    }

    async fn record_ping(&self, check: &check::Ping, res: check::PingResult) -> Result<(), Error> {
        let db = self.clone();
        match res {
            check::PingResult::Reply { reply, latency } => {
                let name = check.name();
                let latency = latency.as_millis() as i64;
                task::spawn_blocking(move || {
                    let conn = db.pool.get().map_err(Error::GetConn)?;
                    conn.execute(
                        "insert into ping_resp (check_name, latency_ms)",
                        (name, latency),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(|err| Err(Error::JoinError(err)))
            }
            check::PingResult::Error { err, latency } => {
                let name = check.name();
                let (kind, err) = match err {
                    ping_rs::PingError::BadParameter(msg) => {
                        ("bad_param", format!("bad param: {msg}"))
                    }
                    ping_rs::PingError::OsError(code, msg) => {
                        ("os_error", format!("os: code: {code} msg: {msg}"))
                    }
                    ping_rs::PingError::IpError(status) => ("ip", status.to_string()),
                    ping_rs::PingError::TimedOut => ("timeout", "timeout".to_string()),
                    ping_rs::PingError::IoPending => ("io", "pending".to_string()),
                    ping_rs::PingError::DataSizeTooBig(max) => {
                        ("too_big", format!("ping payload too big (max: {max})"))
                    }
                };
                task::spawn_blocking(move || {
                    let conn = db.pool.get().map_err(Error::GetConn)?;
                    conn.execute(
                        "insert into ping_resp (check_name, error, error_kind)",
                        (name, err, kind),
                    )?;
                    Ok(())
                })
                .await
                .unwrap_or_else(|err| Err(Error::JoinError(err)))
            }
        }
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
    use rusqlite::Connection;

    #[test]
    fn migrations() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate::migrations::runner().run(&mut conn).unwrap();
    }

    #[tokio::test]
    async fn ensure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.as_ref().join("test.db");
        let db = Db::connect(&path).await.unwrap();
        let check = check::Ping {
            name: String::from("ping-name"),
            host: String::from("ping-host"),
        };
        let dbcheck = db.ensure_check(&check).await.unwrap();
        assert_eq!(dbcheck, DbCheck::new(&check, 1));
        let dbcheck = db.ensure_check(&check).await.unwrap();
        assert_eq!(dbcheck, DbCheck::new(&check, 1));
    }
}
