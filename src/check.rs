use core::net;
use std::{
    fmt::Display,
    io,
    net::{AddrParseError, IpAddr},
    process::Output,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::{extract, response::IntoResponse, routing};
use futures::{Future, TryFutureExt};
use reqwest::{Method, StatusCode};
use rusqlite::types::FromSql;
use serde::{Deserialize, Serialize};
use tokio::{
    task::{spawn_blocking, JoinError, JoinSet},
    time::error::Elapsed,
};
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

    #[error("task panicked: {0}")]
    UnexpectedPanic(#[source] JoinError),

    #[error("http or checker quit unexpectedly")]
    UnexpectedQuit,

    #[error("http quit unexpectedly")]
    HttpQuit,

    #[error("axum failure: {0}")]
    Axum(#[source] io::Error),

    #[error("could not bind to http addr: {0}")]
    BindHttp(#[source] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error(transparent)]
    Error(#[from] Error),
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            ApiError::Error(_) => (StatusCode::INTERNAL_SERVER_ERROR, "").into_response(),
        }
    }
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
    checks: Vec<Check>,
}

#[derive(Clone)]
struct Context {
    timeout: Duration,
    db: db::Db,
}

/// for fetching metrics from the sqlite db.
#[derive(Debug, Deserialize)]
#[serde(default)]
struct MetricsQuery {
    range: i32,
}

#[derive(Debug, Serialize)]
struct Metrics {
    nums: Vec<i32>,
}

impl Default for MetricsQuery {
    fn default() -> Self {
        MetricsQuery { range: 42 }
    }
}

#[derive(Debug, Clone)]
enum Check {
    Http(Http),
    Ping(Ping),
}

impl Check {
    fn name(&self) -> &str {
        match self {
            Check::Http(c) => &c.name,
            Check::Ping(c) => &c.name,
        }
    }
    fn kind(&self) -> Kind {
        match self {
            Check::Http(_) => Kind::Http,
            Check::Ping(_) => Kind::Ping,
        }
    }

    async fn check(&self, ctx: Context) {
        match self {
            Check::Http(http) => http.check(ctx).await,
            Check::Ping(ping) => ping.check(ctx).await,
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
            checks.push(Check::Http(http));
        }
        for (name, ping) in &config.ping {
            let ping = Ping::build(name, ping, &db).await?;
            checks.push(Check::Ping(ping));
        }
        Ok(Self {
            db,
            config: config.clone(),
            checks,
        })
    }

    #[instrument(skip_all)]
    pub async fn run(&self) -> Result<(), Error> {
        let mut tasks = JoinSet::default();
        {
            let checker = self.clone();
            tasks.spawn(async move { checker.listen().await });
        }
        {
            let checker = self.clone();
            tasks.spawn(async move { checker.check_loop().await });
        }
        while let Some(res) = tasks.join_next().await {
            return res
                .map_err(Error::UnexpectedPanic)?
                .or(Err(Error::UnexpectedQuit));
        }
        Ok(())
    }

    async fn listen(&self) -> Result<(), Error> {
        tracing::info!("Starting http listener on {}", self.config.listen);
        let app = axum::Router::new();
        let app = app.route("/", routing::get(|| async { "Hello, World." }));
        let app = app.route("/foo", routing::get(|| async { "bar" }));
        let data = self.clone();
        let app = app.route(
            "/query",
            routing::get(
                |extract::Query(query): extract::Query<MetricsQuery>| async move {
                    data.query(query).await
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind(&self.config.listen)
            .await
            .map_err(Error::BindHttp)?;
        axum::serve(listener, app)
            .await
            .map_err(Error::Axum)
            .or(Err(Error::HttpQuit))
    }

    // fetches data from the sqlite db according to request
    async fn query(&self, query: MetricsQuery) -> Result<axum::Json<Metrics>, ApiError> {
        let metrics = Metrics {
            nums: vec![1, 2, 3, 4, 5],
        };
        let resp = axum::Json(metrics);
        Ok(resp)
    }

    async fn check_loop(&self) -> Result<(), Error> {
        loop {
            let now = Instant::now();
            self.check().await?;
            let sleep = self.config.interval.saturating_sub(now.elapsed());
            tokio::time::sleep(sleep).await;
        }
    }

    /// runs all of the checks once.
    async fn check(&self) -> Result<(), Error> {
        let mut tasks = JoinSet::default();
        tracing::info!("Spawning {} checks...", self.checks.len());
        for check in &self.checks {
            let check = check.clone();
            let ctx = Context {
                timeout: self.config.interval,
                db: self.db.clone(),
            };
            tasks.spawn(async move { check.check(ctx).await });
        }
        while let Some(res) = tasks.join_next().await {
            if let Err(join_err) = res {
                tracing::error!("check task panicked: {join_err}")
            }
        }
        tracing::info!("{} checks completed.", self.checks.len());
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
    async fn check(&self, ctx: Context) {
        let http_res = tokio::time::timeout(ctx.timeout, self.attempt(ctx.timeout))
            .await
            .unwrap_or_else(|err| Err(HttpError::TaskTimeout(err)));
        match &http_res {
            Ok(_) => {
                tracing::info!("Http ok");
            }
            Err(err) => {
                tracing::error!("Http failed: {err}")
            }
        };
        if let Err(err) = ctx.db.record_http(self, http_res).await {
            tracing::error!("failed to record http result: {err}");
        }
    }

    /// does one check. an Err variant is an application level error. the HttpResult will contain
    /// any sort of http related error.
    async fn attempt(&self, timeout: Duration) -> Result<HttpResult, HttpError> {
        let client: reqwest::Client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(HttpError::BuildHttpClient)?;
        let req = client
            .request(Method::GET, self.url.as_ref())
            .build()
            .map_err(HttpError::BuildHttpRequest)?;
        let start = Instant::now();
        let res = client
            .execute(req)
            .await
            .map(|resp| HttpResult {
                resp,
                latency: start.elapsed(),
            })
            .map_err(|err| HttpError::Error {
                err,
                latency: start.elapsed(),
            });
        res
    }
}

#[derive(Debug)]
pub struct HttpResult {
    pub resp: reqwest::Response,
    pub latency: Duration,
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

    #[error("ping task failed to complete")]
    TaskTimeout(Elapsed),
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
    async fn check(&self, ctx: Context) {
        let ping_res = tokio::time::timeout(ctx.timeout, self.attempt())
            .await
            .unwrap_or_else(|err| Err(PingError::TaskTimeout(err)));
        match &ping_res {
            Ok(_) => {
                tracing::info!("Ping ok");
            }
            Err(err) => {
                tracing::error!("Ping failed: {err}")
            }
        };
        if let Err(err) = ctx.db.record_ping(self, ping_res).await {
            tracing::error!("could not record ping result: {err}");
        }
    }

    #[instrument(skip_all)]
    async fn attempt(&self) -> Result<PingResult, PingError> {
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
