use std::time::Instant;

use async_trait::async_trait;
use reqwest::Method;
use tracing::instrument;

use crate::config;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("could not parse url: {0}")]
    ParseUrl(#[source] url::ParseError),

    #[error("could not build http client: {0}")]
    BuildHttpClient(#[source] reqwest::Error),

    #[error("could not build http request: {0}")]
    BuildHttpRequest(#[source] reqwest::Error),
}

pub struct Opts {}

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

    async fn check(&self) -> Result<HttpResult, Error> {
        let client: reqwest::Client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Error::BuildHttpClient)?;
        let req = client
            .request(Method::GET, self.url.as_ref())
            .build()
            .map_err(Error::BuildHttpRequest)?;
        client
            .execute(req)
            .await
            .map(|resp| HttpResult::Response { resp })
            .or_else(|err| Ok(HttpResult::Error { err }))
    }
}

enum HttpResult {
    Response { resp: reqwest::Response },
    Error { err: reqwest::Error },
}

pub struct Ping {
    name: String,
    host: String,
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
        todo!()
    }
}

struct PingResult {}
