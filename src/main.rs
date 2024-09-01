use clap::Parser;
use color_eyre::eyre::{self, Context};
use eyre::eyre;
use r2d2_sqlite::SqliteConnectionManager;
use reqwest::Method;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

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

    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(tracing_error::ErrorLayer::default())
        .init();

    let args = Args::parse();
    let config = Config::from_path(&args.config).await?;
    let checks = config.checks();
    tracing::info!(config = %args.config.display(), "Loaded {} checks.", checks.len());
    let db = Db::connect(&args.db)?;
    let mut tasks = tokio::task::JoinSet::new();
    for check in checks.clone() {
        let checker = Checker::new(&config, &db, &check);
        tasks.spawn(async move {
            checker
                .run()
                .await
                .map_err(|err| eyre!("check failed: {err}"))
                .map_or_else(|e| e, |_| eyre::eyre!("{check} quit unexpectedly"))
        });
    }
    while let Some(res) = tasks.join_next().await {
        let err = res.wrap_err("check panicked")?;
        eyre::bail!(err);
    }
    Ok(())
}

#[allow(unused)]
#[derive(Debug)]
struct Checker {
    cfg: Config,
    db: Db,
    check: Check,
}

impl Checker {
    fn new(cfg: &Config, db: &Db, check: &Check) -> Self {
        Self {
            cfg: cfg.clone(),
            db: db.clone(),
            check: check.clone(),
        }
    }

    async fn run(&self) -> eyre::Result<()> {
        loop {
            match &self.check {
                Check::Ping(ping) => self.check_ping(ping).await?,
                Check::Http(http) => self.check_http(http).await?,
            }
            tracing::info!("Running check {}", self.check);
            tokio::time::sleep(self.cfg.interval).await;
        }
    }

    async fn check_ping(&self, _ping: &Ping) -> eyre::Result<()> {
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn check_http(&self, http: &Http) -> eyre::Result<()> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .wrap_err("build client")?;
        let req = client
            .request(Method::GET, &http.url)
            .timeout(Duration::from_secs(1))
            .build()
            .map_err(|err| eyre!("build new request: {err}"))?;
        match client
            .execute(req)
            .await
            .map_err(|err| eyre!("execute: {err:?}"))
        {
            Ok(resp) => {
                tracing::info!("{} -> {}", self.check, resp.status());
            }
            Err(err) => {
                tracing::error!("{} 💥 {err}", self.check);
            }
        }
        Ok(())
    }
}

type DbPool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone, Debug)]
#[allow(unused)]
struct Db {
    pool: Arc<DbPool>,
}

impl Db {
    fn connect(path: &Path) -> eyre::Result<Self> {
        Self::migrate(path).wrap_err("migrate db")?;
        let mgr = SqliteConnectionManager::file(path);
        let pool = r2d2::Pool::new(mgr).wrap_err("create db pool")?;
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

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Check {
    Ping(Ping),
    Http(Http),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Ping {
    name: String,
    host: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Http {
    name: String,
    url: String,
    code: Option<u32>,
}

impl Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Check::Ping(Ping { name, .. }) => write!(f, "{name} (ping)"),
            Check::Http(Http { name, .. }) => write!(f, "{name} (http)"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Config {
    #[serde(with = "humantime_serde")]
    interval: Duration,
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
            .map(|(name, cfg)| {
                Check::Ping(Ping {
                    name,
                    host: cfg.host,
                })
            })
            .chain(self.http.clone().into_iter().map(|(name, cfg)| {
                Check::Http(Http {
                    name,
                    url: cfg.url,
                    code: cfg.code,
                })
            }))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PingConfig {
    host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HttpConfig {
    url: String,
    code: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn migrations() {
        let mut conn = Connection::open_in_memory().unwrap();
        db::migrations::runner().run(&mut conn).unwrap();
    }

    #[test]
    fn config_serde() {
        let config = Config::from_str(include_str!("../checks.toml")).unwrap();
        assert_eq!(config.interval, Duration::from_secs(1));
        let checks = config.checks();
        assert_eq!(
            checks,
            HashSet::from([
                Check::Ping(Ping {
                    name: String::from("google"),
                    host: String::from("google.com")
                }),
                Check::Ping(Ping {
                    name: String::from("yahoo"),
                    host: String::from("yahoo.com")
                }),
                Check::Http(Http {
                    name: String::from("google"),
                    url: String::from("https://google.com"),
                    code: None,
                })
            ])
        );
    }
}
