#![allow(unused)]

use clap::Parser;
use color_eyre::eyre::{self, Context, Error};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use tracing::info;
use tracing_subscriber::fmt::init;

#[derive(clap::Parser)]
struct Args {
    #[arg(long, default_value = "checks.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt().init();
    let args = Args::parse();
    let config = Config::from_path(&args.config).await?;
    let checks = config.checks();
    info!(config = %args.config.display(), "Loaded {} checks.", checks.len());
    Ok(())
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
    async fn from_path(p: &Path) -> eyre::Result<Self> {
        tokio::fs::read(p)
            .await
            .wrap_err_with(|| format!("read config file at {}", p.display()))
            .and_then(|bs| String::from_utf8(bs).wrap_err("config to string"))
            .and_then(Self::from_str)
    }

    fn from_str(s: impl AsRef<str>) -> eyre::Result<Self> {
        toml::from_str(s.as_ref()).wrap_err("unmarshal toml")
    }

    fn checks(&self) -> HashSet<Check> {
        self.ping
            .clone()
            .into_iter()
            .map(|(name, cfg)| Check::Ping {
                name,
                host: cfg.host,
            })
            .chain(
                self.http
                    .clone()
                    .into_iter()
                    .map(|(name, cfg)| Check::Http {
                        name,
                        host: cfg.host,
                        port: cfg.port,
                        code: cfg.code,
                    }),
            )
            .collect()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PingConfig {
    host: String,
}

#[derive(Clone, Serialize, Deserialize)]
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
            .checks();
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
