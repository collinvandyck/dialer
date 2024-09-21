use core::net;
use std::{
    net::{AddrParseError, IpAddr},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use ping_rs::{PingError, PingOptions, PingReply};
use reqwest::Method;
use tokio::task::spawn_blocking;
use tracing::instrument;

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
