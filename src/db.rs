use anyhow::{Context, Result};
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::{path::Path, sync::Arc};

type DbPool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone, Debug)]
pub struct Db {
    pool: Arc<DbPool>,
}

/// represents a results record
#[derive(Debug)]
pub struct Record {
    pub name: String,
    pub kind: String,
    pub epoch: u64,
    pub ms: u64,
}

impl Db {
    pub async fn connect(path: &Path) -> Result<Self> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            Self::migrate(&path)?;
            let mgr = SqliteConnectionManager::file(path);
            let pool = r2d2::Pool::new(mgr).context("could not create db pool")?;
            let pool = Arc::new(pool);
            Ok(Db { pool })
        })
        .await?
    }

    /// migrates the db at the specified path. is not compatible with the sqlite pool so we open a
    /// connection manually.
    fn migrate(path: &Path) -> Result<()> {
        let mut conn = Connection::open(path)?;
        migrate::migrations::runner().run(&mut conn)?;
        Ok(())
    }

    pub fn conn(&self) -> Result<PooledConnection<SqliteConnectionManager>> {
        Ok(self.pool.get()?)
    }

    pub async fn with_conn<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: Fn(PooledConnection<SqliteConnectionManager>) -> anyhow::Result<R>,
        F: Send + Sync + 'static,
        R: Send + Sync + 'static,
    {
        let db = self.clone();
        let res = tokio::task::spawn_blocking(move || {
            let conn = db.conn().context("get conn")?;
            f(conn)
        })
        .await
        .context("blocking thread panicked")?;
        res
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
}
