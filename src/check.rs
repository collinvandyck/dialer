use core::net;
use std::{
    net::{AddrParseError, IpAddr},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use ping_rs::{PingError, PingOptions, PingReply};
use reqwest::Method;
use tokio::task::spawn_blocking;
use tracing::{info, instrument};

use crate::config;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("could not parse url: {0}")]
    ParseUrl(#[source] url::ParseError),

    #[error("could not parse host: {0}")]
    ParseIpAddr(#[source] AddrParseError),

    #[error("could not build http client: {0}")]
    BuildHttpClient(#[source] reqwest::Error),

    #[error("could not build http request: {0}")]
    BuildHttpRequest(#[source] reqwest::Error),

    #[error("could not connect to db: {0}")]
    DbConnect(#[source] crate::db::Error),

    #[error("could not persist check: {0}")]
    EnsureCheck(#[source] crate::db::Error),
}

#[derive(Clone)]
pub struct Checker {
    db: crate::db::Db,
    config: crate::config::Config,
}

impl Checker {
    pub async fn from_config(config: &config::Config) -> Result<Self, Error> {
        let db = crate::db::Db::connect(&config.db_path)
            .await
            .map_err(Error::DbConnect)?;
        let https = config
            .http
            .iter()
            .map(Http::try_from)
            .collect::<Result<Vec<_>, Error>>()?;
        let pings = config
            .ping
            .iter()
            .map(Ping::try_from)
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self {
            db,
            config: config.clone(),
        })
    }

    pub async fn run(&self) -> Result<(), Error> {
        loop {
            let now = Instant::now();
            self.clone().run_once().await?;
            let sleep = self.config.interval.saturating_sub(now.elapsed());
            tokio::time::sleep(sleep).await;
            tracing::info!("hi");
        }
    }

    async fn run_once(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Http,
    Ping,
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
    name: String,
    url: reqwest::Url,
    code: Option<u32>,
}

impl TryFrom<(&String, &config::Http)> for Http {
    type Error = Error;
    fn try_from((name, http): (&String, &config::Http)) -> Result<Self, Self::Error> {
        let url = reqwest::Url::parse(&http.url).map_err(Error::ParseUrl)?;
        Ok(Self {
            name: name.to_string(),
            url,
            code: http.code.clone(),
        })
    }
}

impl Http {
    fn from_config(name: &str, http: &config::Http) -> Result<Self, Error> {
        let url = reqwest::Url::parse(&http.url).map_err(Error::ParseUrl)?;
        Ok(Self {
            name: name.to_string(),
            url,
            code: http.code.clone(),
        })
    }

    /// does one check. an Err variant is an application level error. the HttpResult will contain
    /// any sort of http related error.
    async fn check(&self) -> Result<HttpResult, Error> {
        let client: reqwest::Client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Error::BuildHttpClient)?;
        let req = client
            .request(Method::GET, self.url.as_ref())
            .build()
            .map_err(Error::BuildHttpRequest)?;
        let start = Instant::now();
        client
            .execute(req)
            .await
            .map(|resp| HttpResult::Response {
                resp,
                latency: start.elapsed(),
            })
            .or_else(|err| {
                Ok(HttpResult::Error {
                    err,
                    latency: start.elapsed(),
                })
            })
    }
}

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
    pub name: String,
    pub host: String,
}

impl TryFrom<(&String, &config::Ping)> for Ping {
    type Error = Error;
    fn try_from((name, ping): (&String, &config::Ping)) -> Result<Self, Self::Error> {
        Ok(Self {
            name: name.to_string(),
            host: ping.host.clone(),
        })
    }
}

impl Ping {
    fn from_config(name: &str, ping: &config::Ping) -> Result<Self, Error> {
        Ok(Self {
            name: name.to_string(),
            host: ping.host.clone(),
        })
    }

    #[instrument(skip_all, fields(kind="ping", name = self.name))]
    async fn check(&self) -> Result<PingResult, Error> {
        let opts = PingOptions {
            ttl: 128,
            dont_fragment: true,
        };
        let addr = self.host.parse().map_err(Error::ParseIpAddr)?;
        // todo: better timeout
        let timeout = Duration::from_secs(1);
        // todo: what should the ping data be
        let data = [1, 2, 3, 4];
        let start = Instant::now();
        tokio::task::spawn_blocking(move || {
            ping_rs::send_ping(&addr, timeout, &data, Some(&opts))
                .map(|reply| PingResult::Reply {
                    reply,
                    latency: start.elapsed(),
                })
                .or_else(|err| {
                    Ok(PingResult::Error {
                        err,
                        latency: start.elapsed(),
                    })
                })
        })
        .await
        .unwrap()
    }
}

pub enum PingResult {
    Reply { reply: PingReply, latency: Duration },
    Error { err: PingError, latency: Duration },
}
