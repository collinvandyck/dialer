//! Defines types and helpers related to getting data out of the db

use std::time::Duration;

use crate::{
    checker,
    config::{self, Config},
    db,
};
use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{self, Query, State},
    response::{IntoResponse, Response},
    routing, Json,
};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use rusqlite::{named_params, params};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;
use tracing::{info, instrument};

/// handles web serving and api requests
#[derive(Clone)]
pub struct Server {
    config: Config,
    db: db::Db,
}

impl Server {
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
            .route("/query", routing::get(handle_metrics))
            .nest_service("/", ServeDir::new("html"))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind(&self.config.listen)
            .await
            .context("could not bind http listener")?;
        info!("Bound http listener to {}", self.config.listen);
        axum::serve(listener, router).await.context("axum failed")?;
        bail!("axum quit unexpectedly");
    }
}

enum ServerError {
    Anyhow(anyhow::Error),
    InvalidEndDate,
}

impl IntoResponse for ServerError {
    #[instrument(skip_all)]
    fn into_response(self) -> Response {
        match self {
            ServerError::Anyhow(err) => {
                tracing::error!("{err}");
                let code = StatusCode::INTERNAL_SERVER_ERROR;
                let res = (code, code.to_string());
                res.into_response()
            }
            Self::InvalidEndDate => {
                tracing::warn!("Invalid end date");
                (StatusCode::BAD_REQUEST, "end date must be after start date").into_response()
            }
        }
    }
}

impl<E> From<E> for ServerError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self::Anyhow(err.into())
    }
}

struct Rollup {
    id: u64,
    name: String,
    kind: String,
    bucket: u64,
    min: u64,
    avg: u64,
    max: u64,
    count: usize,
    errs: usize,
}

trait DateTimeExt {
    fn epoch_secs(&self) -> Result<u64>;
}

impl DateTimeExt for DateTime<Utc> {
    fn epoch_secs(&self) -> Result<u64> {
        Ok((*self - DateTime::UNIX_EPOCH).to_std()?.as_secs())
    }
}

trait DurationExt {
    // the resolution of a rollup is based on the duration of the window
    fn window_resolution(&self) -> Duration;
}

impl DurationExt for Duration {
    fn window_resolution(&self) -> Duration {
        // for a 10m window use a 1s resolution
        if *self <= Duration::from_secs(60 * 10) {
            return Duration::from_secs(1);
        }
        // fallback of 5s resolution
        Duration::from_secs(5)
    }
}

#[instrument(skip_all)]
async fn handle_metrics(
    State(Server { config, db }): State<Server>,
    Query(mut query): Query<MetricsQuery>,
) -> Result<Json<Metrics>, ServerError> {
    let now = Utc::now();
    if let Some(last) = query.last {
        query.start = Some(now - last);
        query.end = Some(now);
    };
    let start = query.start.unwrap_or(now - Duration::from_secs(3600));
    let end = query.end.unwrap_or(now);
    if end <= start {
        return Err(ServerError::InvalidEndDate);
    }
    let window = (end - start).to_std()?;
    let resolution = window.window_resolution();
    let metrics = db
        .with_conn(move |conn| {
            let mut metrics = Metrics::default();
            let mut rows = conn.prepare_cached(
                "
                    SELECT
                        r.check_id,
                        c.name,
                        c.kind,
                        r.epoch / :rollup * :rollup AS bucket,
                        datetime(r.epoch / :rollup * :rollup, 'unixepoch') as time,
                        CAST(MIN(r.ms) as INTEGER) AS min,
                        CAST(AVG(r.ms) as INTEGER) AS avg,
                        CAST(MAX(r.ms) as INTEGER) AS max,
                        COUNT(*) AS count,
                        COUNT(r.err) AS errs
                    FROM results r
                    JOIN checks c on r.check_id = c.id
                    WHERE r.epoch >= :start_time
                    AND r.epoch <= :end_time
                    GROUP BY r.check_id, c.name, c.kind, bucket
                    ORDER BY bucket, name, kind
                    ",
            )?;
            let start = start.epoch_secs()?;
            let end = end.epoch_secs()?;
            let params = named_params! {
                ":rollup": resolution.as_secs(),
                ":start_time": start,
                ":end_time": end,
            };
            let rows = rows.query_map(params, |row| {
                Ok(Rollup {
                    id: row.get("check_id")?,
                    name: row.get("name")?,
                    kind: row.get("kind")?,
                    bucket: row.get("bucket")?,
                    min: row.get("min")?,
                    max: row.get("max")?,
                    avg: row.get("avg")?,
                    count: row.get("count")?,
                    errs: row.get("errs")?,
                })
            })?;
            for row in rows {
                let row = row?;
                let kind = checker::Kind::try_from(row.kind.as_str())?;
                let series = metrics.get_mut(&row.name, kind);
                let ts = chrono::DateTime::from_timestamp(row.bucket as i64, 0)
                    .context("could not convert epoch to timestamp")?;
                series.values.push(TimeValue {
                    ts,
                    ms: Some(row.avg),
                    err: None,
                });
            }
            Ok(metrics)
        })
        .await?;
    Ok(Json(metrics))
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct MetricsQuery {
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    #[serde(with = "humantime_serde")]
    last: Option<Duration>,
}

impl std::fmt::Debug for MetricsQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut f = f.debug_struct("Query");
        if let Some(start) = &self.start {
            f.field("start", start);
        }
        if let Some(end) = &self.end {
            f.field("end", end);
        }
        f.finish_non_exhaustive()
    }
}

impl Default for MetricsQuery {
    fn default() -> Self {
        Self {
            start: None,
            end: None,
            last: None,
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
    pub ts: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "ms")]
    pub ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<Error>,
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub msg: String,
    pub kind: String,
}
