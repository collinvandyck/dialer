use core::net;
use std::{
    fmt::Display,
    io,
    net::{AddrParseError, IpAddr},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use reqwest::Method;
use rusqlite::types::FromSql;
use tokio::task::{spawn_blocking, JoinError, JoinSet};
use tracing::{info, instrument};

use crate::{config, db};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("could not parse url: {0}")]
    ParseUrl(#[source] url::ParseError),

    #[error("could not connect to db: {0}")]
    DbConnect(#[source] crate::db::Error),

    #[error("could not persist check: {0}")]
    EnsureCheck(#[source] crate::db::Error),

    #[error("could not materialize check: {0}")]
    Materialize(#[source] crate::db::Error),

    #[error("check panicked: {0}")]
    CheckPanic(#[source] tokio::task::JoinError),
}

pub async fn run(config: &config::Config) -> Result<(), Error> {
    let checker = Checker::from_config(config).await?;
    checker.run().await?;
    Ok(())
}

#[derive(Clone)]
pub struct Checker {
    db: crate::db::Db,
    config: crate::config::Config,
    checks: Vec<ACheck>,
}

#[derive(Debug, Clone)]
enum ACheck {
    Http(Http),
    Ping(Ping),
}

impl ACheck {
    fn name(&self) -> &str {
        match self {
            ACheck::Http(c) => &c.name,
            ACheck::Ping(c) => &c.name,
        }
    }
    fn kind(&self) -> Kind {
        match self {
            ACheck::Http(_) => Kind::Http,
            ACheck::Ping(_) => Kind::Ping,
        }
    }
    async fn run(&self) -> Result<(), Error> {
        match self {
            ACheck::Http(http) => http.run().await,
            ACheck::Ping(ping) => ping.run().await,
        }
    }
}

impl Checker {
    async fn from_config(config: &config::Config) -> Result<Self, Error> {
        let db = crate::db::Db::connect(&config.db_path)
            .await
            .map_err(Error::DbConnect)?;
        let mut checks = vec![];
        for (name, http) in &config.http {
            let http = Http::build(name, http, &db).await?;
            checks.push(ACheck::Http(http));
        }
        for (name, ping) in &config.ping {
            let ping = Ping::build(name, ping, &db).await?;
            checks.push(ACheck::Ping(ping));
        }
        Ok(Self {
            db,
            config: config.clone(),
            checks,
        })
    }

    pub async fn run(&self) -> Result<(), Error> {
        loop {
            let now = Instant::now();
            self.run_once().await?;
            let sleep = self.config.interval.saturating_sub(now.elapsed());
            tokio::time::sleep(sleep).await;
        }
    }

    async fn run_once(&self) -> Result<(), Error> {
        let mut tasks = JoinSet::default();
        tracing::info!("Spawning {} checks...", self.checks.len());
        for check in &self.checks {
            let check = check.clone();
            let checker = self.clone();
            tasks.spawn(async move { checker.run_check_once(&check).await });
        }
        while let Some(res) = tasks.join_next().await {
            let res = res.map_err(Error::CheckPanic)?;
            if let Err(err) = res {
                tracing::error!("check failed: {err}");
            }
        }
        tracing::info!("{} checks completed.", self.checks.len());
        Ok(())
    }

    async fn run_check_once(&self, check: &ACheck) -> Result<(), Error> {
        let name = check.name();
        let kind = check.kind();
        tracing::info!("check: {kind}:{name}");
        check.run().await?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Http,
    Ping,
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

pub trait Check: Clone + Send + Sync {
    fn name(&self) -> String;
    fn kind(&self) -> Kind;
}

impl Check for Http {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> Kind {
        Kind::Http
    }
}

impl Check for Ping {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> Kind {
        Kind::Ping
    }
}

pub struct Opts {}

#[derive(Debug, Clone)]
pub struct Http {
    id: u64,
    name: String,
    url: reqwest::Url,
    code: Option<u32>,
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
}

impl Http {
    async fn build(name: &str, http: &config::Http, db: &db::Db) -> Result<Self, Error> {
        let url = reqwest::Url::parse(&http.url).map_err(Error::ParseUrl)?;
        let id = db
            .materialize(name, Kind::Http)
            .await
            .map_err(Error::Materialize)?;
        Ok(Self {
            id,
            name: name.to_string(),
            url,
            code: http.code.clone(),
        })
    }

    #[instrument(skip_all, fields(kind="ping", name = self.name))]
    async fn run(&self) -> Result<(), Error> {
        let http_res = self.check().await;
        match http_res {
            Ok(_) => {
                tracing::info!("Http ok");
            }
            Err(err) => {
                tracing::error!("Http failed: {err}")
            }
        };
        Ok(())
    }

    /// does one check. an Err variant is an application level error. the HttpResult will contain
    /// any sort of http related error.
    async fn check(&self) -> Result<HttpResult, HttpError> {
        let client: reqwest::Client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(HttpError::BuildHttpClient)?;
        let req = client
            .request(Method::GET, self.url.as_ref())
            .build()
            .map_err(HttpError::BuildHttpRequest)?;
        let start = Instant::now();
        client
            .execute(req)
            .await
            .map(|resp| HttpResult::Response {
                resp,
                latency: start.elapsed(),
            })
            .map_err(|err| HttpError::Error {
                err,
                latency: start.elapsed(),
            })
    }
}

#[derive(Debug, strum_macros::Display)]
pub enum HttpResult {
    Response {
        resp: reqwest::Response,
        latency: Duration,
    },
    Error {
        err: reqwest::Error,
        latency: Duration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub id: u64,
    pub name: String,
    pub host: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PingError {
    #[error("could not resolve host '{host}': {err}")]
    ResolveHost {
        host: String,
        #[source]
        err: io::Error,
    },

    #[error("no ip found for host '{host}'")]
    NoIpForHost { host: String },

    #[error("ping failed: {err}")]
    Ping { err: surge_ping::SurgeError },
}

impl Ping {
    async fn build(name: &str, ping: &config::Ping, db: &db::Db) -> Result<Self, Error> {
        let id = db
            .materialize(name, Kind::Ping)
            .await
            .map_err(Error::Materialize)?;
        Ok(Self {
            id,
            name: name.to_string(),
            host: ping.host.clone(),
        })
    }

    #[instrument(skip_all, fields(kind="ping", name = self.name))]
    async fn run(&self) -> Result<(), Error> {
        let ping_res = self.check().await;
        match ping_res {
            Ok(_) => {
                tracing::info!("Ping ok");
            }
            Err(err) => {
                tracing::error!("Ping failed: {err}")
            }
        };
        Ok(())
    }

    #[instrument(skip_all)]
    async fn check(&self) -> Result<PingResult, PingError> {
        // TODO: use a timeout
        let data = [1, 2, 3, 4];
        let addr: IpAddr = {
            match self.host.parse() {
                Ok(ip) => ip,
                Err(_) => {
                    // resolve it as a hostname
                    let ips = dns_lookup::lookup_host(&self.host).map_err(|err| {
                        PingError::ResolveHost {
                            host: self.host.clone(),
                            err,
                        }
                    })?;
                    let addr = ips
                        .into_iter()
                        .filter(|ip| ip.is_ipv4())
                        .next()
                        .ok_or_else(|| PingError::NoIpForHost {
                            host: self.host.clone(),
                        })?;
                    tracing::info!("Resolved {} to ip {addr}", self.host);
                    addr
                }
            }
        };
        surge_ping::ping(addr, &data)
            .await
            .map(|(packet, latency)| PingResult { packet, latency })
            .map_err(|err| PingError::Ping { err })
    }
}

#[derive(Debug)]
pub struct PingResult {
    pub packet: surge_ping::IcmpPacket,
    pub latency: Duration,
}
