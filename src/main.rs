#![allow(unused)]

use chrono::{DateTime, NaiveDateTime};
use clap::Parser;
use color_eyre::eyre::{self, Context};
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use reqwest::Method;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    fmt::Display,
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use strum_macros::EnumString;
use tokio::time::error::Elapsed;
use tracing::info;
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
    config.materialize(db.clone()).await?;
    let checker = Checker::new(&config, &db, checks);
    checker.run().await?;
    Ok(())
}

type DbPool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone, Debug)]
#[allow(unused)]
struct Db {
    pool: Arc<DbPool>,
}

struct DbCheck {
    id: u64,
    name: String,
    kind: String,
    check: Check,
}

impl Db {
    fn ensure_check(&self, check: &Check) -> eyre::Result<DbCheck> {
        let conn = self.get()?;
        let name = check.name();
        let kind = check.kind();
        if let Some(check) = conn
            .query_row(
                "select * from checks where name=?1 and kind=?2",
                (name, kind),
                |row| {
                    Ok(DbCheck {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        kind: row.get(2)?,
                        check: check.clone(),
                    })
                },
            )
            .optional()?
        {
            return Ok(check);
        }
        Ok(conn.query_row(
            "insert into checks (name,kind) values (?1, ?2) returning *",
            (name, kind),
            |row| {
                Ok(DbCheck {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    check: check.clone(),
                })
            },
        )?)
    }

    fn record(&self, check: &Check, result: &CheckResult) -> eyre::Result<()> {
        let name = check.name();
        match result {
            CheckResult::Http(HttpResult::Response { code, latency }) => {
                let conn = self.get()?;
                conn.execute(
                    "
                    insert into http_resp
                    (check_name, latency_ms, code)
                    values
                    (?1, ?2, ?3)
                    ",
                    (check.name(), latency.as_millis() as i64, code),
                )?;
                Ok(())
            }
            CheckResult::Http(HttpResult::Error(err)) => {
                let kind = err.kind.to_string();
                let err = err.to_string();
                let conn = self.get()?;
                conn.execute(
                    "
                    insert into http_resp
                    (check_name, error, error_kind)
                    values
                    (?1, ?2, ?3)
                    ",
                    (check.name(), err, kind),
                )?;
                Ok(())
            }
            CheckResult::Ping(_) => Ok(()),
            CheckResult::Timeout(elapsed) => todo!(),
        }
    }

    fn get(&self) -> eyre::Result<PooledConnection<SqliteConnectionManager>> {
        self.pool.get().wrap_err("get conn")
    }

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

#[allow(unused)]
#[derive(Clone, Debug)]
struct Checker {
    cfg: Config,
    db: Db,
    checks: HashSet<Check>,
}

enum CheckResult {
    Ping(PingResult),
    Http(HttpResult),
    Timeout(Elapsed),
}

impl CheckResult {
    async fn record(&self) -> eyre::Result<()> {
        Ok(())
    }
}

struct PingResult {}

#[derive(Debug)]
enum HttpResult {
    // could not make the request
    Error(ReqwestError),
    // we got a response
    Response { code: u16, latency: Duration },
}

#[derive(Debug)]
struct ReqwestError {
    err: reqwest::Error,
    kind: RequestErrorKind,
}

impl Deref for ReqwestError {
    type Target = reqwest::Error;
    fn deref(&self) -> &Self::Target {
        &self.err
    }
}

impl From<reqwest::Error> for ReqwestError {
    fn from(err: reqwest::Error) -> Self {
        let kind = (&err).into();
        Self { err, kind }
    }
}

#[derive(Debug, Clone, Copy, strum_macros::Display)]
enum RequestErrorKind {
    Status,
    Body,
    Decode,
    Unknown,
}

impl From<&reqwest::Error> for RequestErrorKind {
    fn from(err: &reqwest::Error) -> Self {
        if err.is_status() {
            RequestErrorKind::Status
        } else if err.is_body() {
            RequestErrorKind::Body
        } else if err.is_decode() {
            RequestErrorKind::Decode
        } else {
            RequestErrorKind::Unknown
        }
    }
}

impl Checker {
    fn new(cfg: &Config, db: &Db, checks: HashSet<Check>) -> Self {
        Self {
            cfg: cfg.clone(),
            db: db.clone(),
            checks: checks.clone(),
        }
    }

    #[tracing::instrument(skip_all)]
    async fn run(self) -> eyre::Result<()> {
        loop {
            self.check_all().await?;
            tokio::time::sleep(self.cfg.interval).await;
        }
    }

    // runs all checks once.
    async fn check_all(&self) -> eyre::Result<()> {
        let mut tasks = tokio::task::JoinSet::new();
        for check in &self.checks {
            let checker = self.clone();
            let check = check.clone();
            tasks.spawn(async move { checker.check(&check).await });
        }
        while let Some(res) = tasks.join_next().await {
            res.wrap_err("check panicked")?.wrap_err("check failed")?;
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(kind=check.kind(), name = check.name()))]
    async fn check(&self, check: &Check) -> eyre::Result<()> {
        tracing::info!("Running");
        let timeout = self.cfg.interval;
        let check_res = tokio::time::timeout(timeout, async move {
            match check {
                Check::Http(http) => self.http(http).await.map(CheckResult::Http),
                Check::Ping(ping) => self.ping(ping).await.map(CheckResult::Ping),
            }
        })
        .await
        .unwrap_or_else(|err| Ok(CheckResult::Timeout(err)))?;
        self.db.record(check, &check_res)
    }

    async fn ping(&self, _ping: &Ping) -> eyre::Result<PingResult> {
        Ok(PingResult {})
    }

    async fn http(&self, http: &Http) -> eyre::Result<HttpResult> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .wrap_err("create http client")?;
        let req = client
            .request(Method::GET, &http.url)
            .build()
            .wrap_err("create http request")?;
        let start = Instant::now();
        let http_result = client
            .execute(req)
            .await
            .map(|resp| {
                let code = resp.status().as_u16();
                HttpResult::Response {
                    code,
                    latency: start.elapsed(),
                }
            })
            .unwrap_or_else(|err| HttpResult::Error(ReqwestError::from(err)));
        Ok(http_result)
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Check {
    Ping(Ping),
    Http(Http),
}

impl Check {
    fn name(&self) -> &str {
        match self {
            Check::Ping(ping) => &ping.name,
            Check::Http(http) => &http.name,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Check::Ping(_) => "ping",
            Check::Http(_) => "http",
        }
    }
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
    /// ensures that the checks are recorded into the sqlite db
    async fn materialize(&self, db: Db) -> eyre::Result<()> {
        for check in self.checks() {
            db.ensure_check(&check)?;
        }
        Ok(())
    }
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
        let config = r#"
            interval = "1s"

            [ping]
            google = { host = "google.com" }
            yahoo = { host = "yahoo.com" }

            [http]
            google = { url = "https://google.com" }
            "#;
        let config = Config::from_str(config).unwrap();
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
