#![allow(unused)]

use clap::Parser;
use color_eyre::eyre::{self, Context, ContextCompat, Error};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::task::JoinSet;
use tracing::info;
use tracing_subscriber::fmt::init;

#[derive(clap::Parser)]
struct Args {
    #[arg(long, default_value = "checks.toml")]
    config: PathBuf,

    #[arg(long, default_value = "checks.db")]
    db: PathBuf,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt().init();
    let args = Args::parse();
    let config = Config::from_path(&args.config).await?;
    let checks = config.checks();
    info!(config = %args.config.display(), "Loaded {} checks.", checks.len());
    let db = Db::connect(&args.db)?;
    let mut tasks = JoinSet::new();
    for check in checks {
        let db = db.clone();
        tasks.spawn(run_check(check, db));
    }
    while let Some(res) = tasks.join_next().await {
        res.wrap_err("join failure")?.wrap_err("check failure")?;
    }
    Ok(())
}

async fn run_check(check: Check, db: Db) -> eyre::Result<()> {
    info!("Running check {check:?}");
    Ok(())
}

type DbPool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone)]
struct Db {
    pool: Arc<DbPool>,
}

impl Db {
    fn connect(path: &Path) -> eyre::Result<Self> {
        Self::migrate(path).wrap_err("migrate db")?;
        let mgr = SqliteConnectionManager::file(path);
        let mut pool = r2d2::Pool::new(mgr).wrap_err("create db pool")?;
        let mut conn = pool.get().wrap_err("get conn for migration")?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    fn migrate(path: &Path) -> eyre::Result<()> {
        let mut conn = Connection::open(path).wrap_err("open conn")?;
        db::migrations::runner()
            .run(&mut conn)
            .wrap_err("migrate db")?;
        Ok(())
    }
}

mod db {
    refinery::embed_migrations!("./migrations");
}

#[derive(Debug, Hash, PartialEq, Eq)]
enum Check {
    Ping {
        name: String,
        host: String,
    },
    Http {
        name: String,
        host: String,
        port: Option<u32>,
        code: Option<u32>,
    },
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Config {
    ping: HashMap<String, PingConfig>,
    http: HashMap<String, HttpConfig>,
}

impl Config {
    async fn from_path(p: &Path) -> eyre::Result<Self> {
        tokio::fs::read(p)
            .await
            .wrap_err_with(|| format!("read config file at {}", p.display()))
            .and_then(|bs| String::from_utf8(bs).wrap_err("config to string"))
            .and_then(Self::from_str)
    }

    fn from_str(s: impl AsRef<str>) -> eyre::Result<Self> {
        toml::from_str(s.as_ref()).wrap_err("unmarshal toml")
    }

    fn checks(&self) -> HashSet<Check> {
        self.ping
            .clone()
            .into_iter()
            .map(|(name, cfg)| Check::Ping {
                name,
                host: cfg.host,
            })
            .chain(
                self.http
                    .clone()
                    .into_iter()
                    .map(|(name, cfg)| Check::Http {
                        name,
                        host: cfg.host,
                        port: cfg.port,
                        code: cfg.code,
                    }),
            )
            .collect()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PingConfig {
    host: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct HttpConfig {
    host: String,
    port: Option<u32>,
    code: Option<u32>,
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn migrations() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::db::migrations::runner().run(&mut conn).unwrap();
    }

    #[test]
    fn config_serde() {
        let checks = Config::from_str(include_str!("../checks.toml"))
            .unwrap()
            .checks();
        assert_eq!(
            checks,
            HashSet::from([
                Check::Ping {
                    name: String::from("google"),
                    host: String::from("google.com")
                },
                Check::Http {
                    name: String::from("google"),
                    host: String::from("google.com"),
                    port: None,
                    code: None,
                }
            ])
        );
    }
}
