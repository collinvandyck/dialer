//! Defines types and helpers related to getting data out of the db

use crate::{
    check,
    db::{self, record},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
    pub fn get_mut(&mut self, name: &str, kind: check::Kind) -> &mut Series {
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

    fn find_pos(&self, name: &str, kind: check::Kind) -> Option<usize> {
        self.series
            .iter()
            .enumerate()
            .find(|(idx, s)| s.name == name && s.kind == kind)
            .map(|(idx, s)| idx)
    }
}

#[derive(Debug, Serialize)]
pub struct Series {
    pub kind: check::Kind,
    pub name: String,
    pub values: Vec<TimeValue>,
}

#[derive(Debug, Serialize)]
pub struct TimeValue {
    pub ts: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "ms")]
    pub latency_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub err: Option<Error>,
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub msg: String,
    pub kind: String,
}
