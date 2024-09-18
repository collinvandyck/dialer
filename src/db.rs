use std::{path::Path, sync::Arc};

use eyre::Context;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use tokio::task::JoinError;

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
}

type DbPool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone, Debug)]
pub struct Db {
    pool: Arc<DbPool>,
}

pub async fn connect(path: &Path) -> Result<Db, Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        migrate(&path)?;
        let mgr = SqliteConnectionManager::file(path);
        let pool = r2d2::Pool::new(mgr).map_err(Error::CreatePool)?;
        let pool = Arc::new(pool);
        Ok(Db { pool })
    })
    .await
    .unwrap()
}

pub async fn connect_simple(path: &Path) -> Result<Db, Error> {
    let path = path.to_path_buf();
    let res = blocking(move || {
        migrate(&path)?;
        let mgr = SqliteConnectionManager::file(path);
        let pool = r2d2::Pool::new(mgr).map_err(Error::CreatePool)?;
        let pool = Arc::new(pool);
        Ok(Db { pool })
    })
    .await;
    todo!()
}

async fn blocking<F, R>(f: F) -> Result<R, Error>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .or_else(|err| Err(Error::JoinError(err)))
}

// todo: use blocking threadpool for db runs
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
