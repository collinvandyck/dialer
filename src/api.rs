//! Defines types and helpers related to getting data out of the db

use std::time::Duration;

use crate::{
    checker,
    config::{self, Config},
    db,
};
use anyhow::{bail, Context, Result};
use axum::routing;
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;
use tracing::{info, instrument};

#[derive(Clone)]
pub struct Api {
    config: Config,
    db: db::Db,
}

impl Api {
    pub fn new(config: &Config, db: db::Db) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            db,
        })
    }

    #[instrument(skip_all)]
    pub async fn run(&self) -> Result<()> {
        info!("Starting http listener on {}", self.config.listen);
        let router = axum::Router::new()
            .route(
                "/query",
                routing::get({
                    //
                    || async { "hello world!" }
                }),
            )
            .nest_service("/", ServeDir::new("html"));
        let listener = tokio::net::TcpListener::bind(&self.config.listen)
            .await
            .context("could not bind http listener")?;
        info!("Bound http listener to {}", self.config.listen);
        axum::serve(listener, router).await.context("axum failed")?;
        bail!("axum quit unexpectedly");
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Query {
    start_time: Option<chrono::DateTime<chrono::Utc>>,
    end_time: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            start_time: None,
            end_time: None,
        }
    }
}

#[derive(Debug, Serialize, Default)]
pub struct Metrics {
    pub series: Vec<Series>,
}

impl Metrics {
    pub fn get_mut(&mut self, name: &str, kind: checker::Kind) -> &mut Series {
        let pos = self.find_pos(name, kind);
        let idx = match pos {
            Some(idx) => idx,
            None => {
                let series = Series {
                    kind,
                    name: name.to_string(),
                    values: Vec::with_capacity(1024),
                };
                self.series.push(series);
                self.series.len() - 1
            }
        };
        &mut self.series[idx]
    }

    fn find_pos(&self, name: &str, kind: checker::Kind) -> Option<usize> {
        self.series
            .iter()
            .enumerate()
            .find(|(_idx, s)| s.name == name && s.kind == kind)
            .map(|(idx, _s)| idx)
    }
}

#[derive(Debug, Serialize)]
pub struct Series {
    pub kind: checker::Kind,
    pub name: String,
    pub values: Vec<TimeValue>,
}

#[derive(Debug, Serialize)]
pub struct TimeValue {
    pub ts: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "ms")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<Error>,
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub msg: String,
    pub kind: String,
}
