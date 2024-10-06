use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub db_path: PathBuf,
    pub live_reload: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(default = "default_listen")]
    pub listen: String,
    pub ping: HashMap<String, Ping>,
    pub http: HashMap<String, Http>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: PathBuf::default(),
            live_reload: cfg!(debug_assertions),
            interval: Duration::default(),
            listen: String::default(),
            ping: HashMap::default(),
            http: HashMap::default(),
        }
    }
}

fn default_listen() -> String {
    String::from("0.0.0.0:3000")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ping {
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Http {
    pub url: String,
    pub code: Option<u32>,
}

impl Config {
    pub async fn from_path(p: impl AsRef<Path>) -> anyhow::Result<Self> {
        let p = p.as_ref();
        tokio::fs::read(p)
            .await
            .with_context(|| format!("read config file at {}", p.display()))
            .and_then(|bs| String::from_utf8(bs).context("config to string"))
            .and_then(|s| s.as_str().try_into().context("parse config"))
    }
}

impl TryFrom<&str> for Config {
    type Error = anyhow::Error;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        toml::from_str(value).context("unmarshal toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn from_path() {
        let p = "foo";
        let err = Config::from_path(p).await.unwrap_err();
        let err = format!("{err:#}");
        assert_eq!(
            err,
            "read config file at foo: No such file or directory (os error 2)"
        );
        let config = r#"
            db_path = "checks.db"
            interval = "1s"
            listen = ":3000"

            [ping]
            google = { host = "google.com" }
            yahoo = { host = "yahoo.com" }

            [http]
            google = { url = "https://google.com" }
            "#;
        let path = tempfile::tempdir().unwrap();
        let path = path.path().join("foo");
        tokio::fs::write(&path, config).await.unwrap();
        let config = Config::from_path(&path).await.unwrap();
        assert_eq!(
            config,
            Config {
                db_path: PathBuf::from("checks.db"),
                interval: Duration::from_secs(1),
                listen: String::from(":3000"),
                ping: HashMap::from([
                    (
                        String::from("google"),
                        Ping {
                            host: String::from("google.com")
                        }
                    ),
                    (
                        String::from("yahoo"),
                        Ping {
                            host: String::from("yahoo.com")
                        }
                    ),
                ]),
                http: HashMap::from([(
                    String::from("google"),
                    Http {
                        url: String::from("https://google.com"),
                        code: None
                    }
                )]),
                ..Default::default()
            }
        );
    }

    #[test]
    fn config_serde() {
        let config = r#"
            db_path = "checks.db"
            interval = "1s"

            [ping]
            google = { host = "google.com" }
            yahoo = { host = "yahoo.com" }

            [http]
            google = { url = "https://google.com" }
            "#;
        let config = Config::try_from(config).unwrap();
        assert_eq!(
            config,
            Config {
                live_reload: true,
                db_path: PathBuf::from("checks.db"),
                interval: Duration::from_secs(1),
                listen: default_listen(),
                ping: HashMap::from([
                    (
                        String::from("google"),
                        Ping {
                            host: String::from("google.com")
                        }
                    ),
                    (
                        String::from("yahoo"),
                        Ping {
                            host: String::from("yahoo.com")
                        }
                    ),
                ]),
                http: HashMap::from([(
                    String::from("google"),
                    Http {
                        url: String::from("https://google.com"),
                        code: None
                    }
                )])
            }
        );
    }
}
