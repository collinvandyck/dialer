use std::{path::Path, sync::Arc};

use eyre::Context;
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension};
use tokio::task::{self, JoinError};

use crate::check::{self, Check};

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

#[derive(Clone, Debug)]
pub struct DbCheck<Check> {
    check: Check,
    id: u64,
}

impl<C: Check> DbCheck<C> {
    fn new(check: C, id: u64) -> Self {
        Self { check, id }
    }

    fn from_id_row(check: &C, row: &rusqlite::Row) -> Result<Self, Error> {
        Ok(Self::new(check.clone(), row.get(0)?))
    }
}

impl Db {
    async fn ensure_check<C>(&self, check: &C) -> Result<DbCheck<C>, Error>
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
}

pub async fn connect(path: &Path) -> Result<Db, Error> {
    let path = path.to_path_buf();
    task::spawn_blocking(move || {
        migrate(&path)?;
        let mgr = SqliteConnectionManager::file(path);
        let pool = r2d2::Pool::new(mgr).map_err(Error::CreatePool)?;
        let pool = Arc::new(pool);
        Ok(Db { pool })
    })
    .await
    .unwrap_or_else(|err| Err(Error::JoinError(err)))
}

/// runs the migrations for the db at the specified path.
fn migrate(path: &Path) -> Result<(), Error> {
    let mut conn = Connection::open(path)?;
    migrate::migrations::runner()
        .run(&mut conn)
        .map_err(Error::Migrate)?;
    Ok(())
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
        let db = connect(&path).await.unwrap();
        let check = check::Ping {
            name: String::from("ping-name"),
            host: String::from("ping-host"),
        };
        let dbcheck = db.ensure_check(&check).await.unwrap();
        assert_eq!(dbcheck.id, 1_u64);
        assert_eq!(dbcheck.check, check);

        let dbcheck = db.ensure_check(&check).await.unwrap();
        assert_eq!(dbcheck.id, 1_u64);
        assert_eq!(dbcheck.check, check);
    }
}
