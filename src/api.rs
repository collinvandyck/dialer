//! Defines types and helpers related to getting data out of the db

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Query {
    pub range: i32,
}

impl Default for Query {
    fn default() -> Self {
        Self { range: 42 }
    }
}

#[derive(Debug, Serialize)]
pub struct Metrics {
    pub nums: Vec<i32>,
}
