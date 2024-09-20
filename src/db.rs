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
        Ok(Self::new(check, row.get(0)?))
    }
}

impl Db {
    async fn ensure_check<C: Check>(&self, check: C) -> Result<DbCheck<C>, Error> {
        let db = self.clone();
        task::block_in_place(move || {
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
                    "insert into checks (name, kind) values (?1, ?2) returning *",
                    (&name, kind),
                    |row| Ok(DbCheck::from_id_row(&check, row)),
                )?
            })
        })
    }
}

pub async fn connect(path: &Path) -> Result<Db, Error> {
    let path = path.to_path_buf();
    task::block_in_place(move || {
        migrate(&path)?;
        let mgr = SqliteConnectionManager::file(path);
        let pool = r2d2::Pool::new(mgr).map_err(Error::CreatePool)?;
        let pool = Arc::new(pool);
        Ok(Db { pool })
    })
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
}
