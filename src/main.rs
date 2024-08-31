#![allow(unused)]

use color_eyre::eyre::{self, Context, Error};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

fn main() {
    println!("Hello, world!");
}

#[derive(Debug, Hash, PartialEq, Eq)]
enum Check {
    Ping {
        name: String,
        host: String,
    },
    Http {
        name: String,
        host: String,
        port: Option<u32>,
        code: Option<u32>,
    },
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Config {
    ping: HashMap<String, PingConfig>,
    http: HashMap<String, HttpConfig>,
}

impl Config {
    fn from_str(s: &str) -> eyre::Result<Self> {
        toml::from_str(s).wrap_err("unmarshal toml")
    }
    fn into_checks(self) -> HashSet<Check> {
        self.ping
            .into_iter()
            .map(|(name, cfg)| Check::Ping {
                name,
                host: cfg.host,
            })
            .chain(self.http.into_iter().map(|(name, cfg)| Check::Http {
                name,
                host: cfg.host,
                port: cfg.port,
                code: cfg.code,
            }))
            .collect()
    }
}

#[derive(Serialize, Deserialize)]
struct PingConfig {
    host: String,
}

#[derive(Serialize, Deserialize)]
struct HttpConfig {
    host: String,
    port: Option<u32>,
    code: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serde() {
        let checks = Config::from_str(include_str!("../checks.toml"))
            .unwrap()
            .into_checks();
        assert_eq!(
            checks,
            HashSet::from([
                Check::Ping {
                    name: String::from("google"),
                    host: String::from("google.com")
                },
                Check::Http {
                    name: String::from("google"),
                    host: String::from("google.com"),
                    port: None,
                    code: None,
                }
            ])
        );
    }
}
