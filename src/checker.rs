use crate::{config, db};
use anyhow::{anyhow, bail, Context, Result};
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use reqwest::Method;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::{
    fmt::Display,
    net::IpAddr,
    time::{Duration, Instant},
};
use tokio::{task::JoinSet, time::error::Elapsed};
use tracing::instrument;

#[derive(Clone, Debug)]
pub struct Checker {
    db: crate::db::Db,
    config: crate::config::Config,
    checks: Vec<Check>,
}

#[derive(Debug, Clone)]
enum Check {
    Http(Http),
    Ping(Ping),
}

impl Checker {
    pub async fn new(db: db::Db, config: &config::Config) -> Result<Self> {
        let mut checker = Self {
            db,
            config: config.clone(),
            checks: vec![],
        };
        for (name, http) in &config.http {
            let id = checker.materialize(name, Kind::Http).await?;
            let http = Http::build(name, http, id).await?;
            checker.checks.push(Check::Http(http));
        }
        for (name, ping) in &config.ping {
            let id = checker.materialize(name, Kind::Ping).await?;
            let ping = Ping::build(name, ping, id).await?;
            checker.checks.push(Check::Ping(ping));
        }
        Ok(checker)
    }

    #[instrument(skip_all)]
    pub async fn run(&self) -> anyhow::Result<()> {
        self.check_loop().await
    }

    async fn check_loop(&self) -> Result<()> {
        loop {
            let now = Instant::now();
            self.check_all().await?;
            let sleep = self.config.interval.saturating_sub(now.elapsed());
            tokio::time::sleep(sleep).await;
        }
    }

    /// runs all of the checks once.
    async fn check_all(&self) -> Result<()> {
        let mut tasks = JoinSet::default();
        for check in &self.checks {
            let checker = self.clone();
            let check = check.clone();
            tasks.spawn(async move { checker.check(&check).await });
        }
        while let Some(res) = tasks.join_next().await {
            match res {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::error!("task failed: {err:?}");
                }
                Err(join_err) => {
                    tracing::error!("check task panicked: {join_err}")
                }
            }
        }
        Ok(())
    }

    async fn check(&self, check: &Check) -> anyhow::Result<()> {
        match check {
            Check::Http(http) => self.check_http(http).await.context("http check failed"),
            Check::Ping(ping) => self.check_ping(ping).await.context("ping check failed"),
        }
    }

    async fn check_http(&self, http: &Http) -> anyhow::Result<()> {
        let timeout = self.config.interval;
        let res = tokio::time::timeout(timeout, async move {
            let client: reqwest::Client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(timeout)
                .build()
                .context("build http client")?;
            let req = client
                .request(Method::GET, http.url.as_ref())
                .build()
                .context("build http req")?;
            let start = Instant::now();
            let resp: reqwest::Response = client.execute(req).await.context("request")?;
            anyhow::Ok((resp, start.elapsed()))
        })
        .await;
        match res {
            Ok(Ok((_, latency))) => {
                self.mark_ok(http.id, latency).await?;
            }
            Ok(Err(err)) => {
                tracing::error!("http: {err:?}");
                self.mark_err(http.id, format!("{err:?}")).await?;
            }
            Err(elapsed) => {
                tracing::error!("http timeout after {elapsed:?}");
                self.mark_err(http.id, "timeout").await?;
            }
        };
        Ok(())
    }

    async fn check_ping(&self, ping: &Ping) -> anyhow::Result<()> {
        let timeout = self.config.interval;
        let res = tokio::time::timeout(timeout, async move {
            let data = [1, 2, 3, 4];
            let addr: IpAddr = {
                match ping.host.parse() {
                    Ok(ip) => ip,
                    Err(_) => {
                        // resolve it as a hostname
                        let ips = dns_lookup::lookup_host(&ping.host).context("lookup host")?;
                        let addr = ips
                            .into_iter()
                            .filter(|ip| ip.is_ipv4())
                            .next()
                            .ok_or_else(|| anyhow!("no ip for host"))?;
                        addr
                    }
                }
            };
            // info!("surge ping addr={addr} data={data:?}");
            surge_ping::ping(addr, &data).await.context("ping failed")
        })
        .await;
        match res {
            Ok(Ok((_, latency))) => {
                self.mark_ok(ping.id, latency).await?;
            }
            Ok(Err(err)) => {
                tracing::error!("ping: {err:?}");
                self.mark_err(ping.id, format!("{err:?}")).await?;
            }
            Err(elapsed) => {
                tracing::error!("ping: timeout after {elapsed:?}");
                self.mark_err(ping.id, "timeout").await?;
            }
        };
        Ok(())
    }

    async fn mark_err(&self, id: u64, err: impl AsRef<str>) -> anyhow::Result<()> {
        let err = err.as_ref().to_string();
        self.with_conn(move |conn| {
            conn.execute(
                "insert into results (check_id, err) values (?1,?2)",
                (id, format!("{err:?}")),
            )?;
            Ok(())
        })
        .await?;
        Ok(())
    }

    async fn mark_ok(&self, id: u64, latency: Duration) -> anyhow::Result<()> {
        let ms = latency.as_millis() as u64;
        self.with_conn(move |conn| {
            conn.execute(
                "insert into results (check_id, ms) values (?1,?2)",
                [id, ms],
            )?;
            Ok(())
        })
        .await?;
        Ok(())
    }

    async fn materialize(&self, name: &str, kind: Kind) -> Result<u64> {
        let name = name.to_string();
        let kind = kind.as_str();
        self.with_conn(move |conn| {
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
    }

    async fn with_conn<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: Fn(PooledConnection<SqliteConnectionManager>) -> anyhow::Result<R>,
        F: Send + Sync + 'static,
        R: Send + Sync + 'static,
    {
        let checker = self.clone();
        let res = tokio::task::spawn_blocking(move || {
            let conn = checker.db.conn().context("get conn")?;
            f(conn)
        })
        .await
        .context("blocking thread panicked")?;
        res
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum Kind {
    #[serde(rename = "http")]
    Http,
    #[serde(rename = "ping")]
    Ping,
}

impl TryFrom<&str> for Kind {
    type Error = anyhow::Error;
    fn try_from(kind: &str) -> Result<Self, Self::Error> {
        match kind {
            "http" => Ok(Self::Http),
            "ping" => Ok(Self::Ping),
            _ => bail!("unknown kind: '{kind}'"),
        }
    }
}

impl Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kind::Http => write!(f, "http"),
            Kind::Ping => write!(f, "ping"),
        }
    }
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Http => "http",
            Kind::Ping => "ping",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Http {
    pub id: u64,
    pub name: String,
    pub url: reqwest::Url,
    pub code: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("could not build http client: {0}")]
    BuildHttpClient(#[source] reqwest::Error),

    #[error("could not build http request: {0}")]
    BuildHttpRequest(#[source] reqwest::Error),

    #[error("failed to make request: {err}")]
    Error {
        #[source]
        err: reqwest::Error,
        latency: Duration,
    },

    #[error("http check failed to complete in time")]
    TaskTimeout(Elapsed),
}

impl Http {
    async fn build(name: &str, http: &config::Http, id: u64) -> Result<Self> {
        let url = reqwest::Url::parse(&http.url).context("could not parse http url")?;
        Ok(Self {
            id,
            name: name.to_string(),
            url,
            code: http.code.clone(),
        })
    }
}

pub enum CheckResult {
    Ok { latency: Duration },
    Err { err: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub id: u64,
    pub name: String,
    pub host: String,
}

impl Ping {
    async fn build(name: &str, ping: &config::Ping, id: u64) -> Result<Self> {
        Ok(Self {
            id,
            name: name.to_string(),
            host: ping.host.clone(),
        })
    }
}
