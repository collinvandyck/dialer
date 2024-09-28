//! Defines types and helpers related to getting data out of the db

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::check;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Query {}

impl Default for Query {
    fn default() -> Self {
        Self {}
    }
}

#[derive(Debug, Serialize, Default)]
pub struct Metrics {
    pub series: Vec<Series>,
}

#[derive(Debug, Serialize)]
pub struct Series {
    kind: check::Kind,
    name: String,
    values: Vec<TimeValue>,
}

#[derive(Debug, Serialize)]
pub struct TimeValue {
    epoch: chrono::DateTime<chrono::Utc>,
    value: Value,
}

#[derive(Debug, Serialize)]
pub enum Value {
    Ok { latency: Duration },
    Err { err: String, kind: String },
}
