use crate::{
    checker::{self, Checker},
    config,
    db::Db,
    web::Server,
};
use anyhow::{anyhow, bail, Result};
use tokio::task::JoinSet;

#[derive(Clone)]
pub struct App {
    api: Server,
    checker: checker::Checker,
}

impl App {
    pub async fn new(config: &config::Config) -> Result<Self> {
        let db = Db::connect(&config.db_path).await?;
        let checker = Checker::new(db.clone(), config).await?;
        let api = Server::new(config, db.clone())?;
        Ok(Self { api, checker })
    }

    pub async fn run(&self) -> Result<()> {
        let mut js = JoinSet::new();
        js.spawn(self.clone().run_checker());
        js.spawn(self.clone().run_api());
        match js.join_next().await {
            Some(Ok(err)) => bail!(err),
            Some(Err(je)) => bail!("panic! {je:#}"),
            None => Ok(()),
        }
    }

    async fn run_api(self) -> anyhow::Error {
        match self.api.run().await {
            Ok(()) => anyhow!("api quit unexpectedly"),
            Err(err) => err.context("api failed"),
        }
    }

    async fn run_checker(self) -> anyhow::Error {
        match self.checker.run().await {
            Ok(()) => anyhow!("checker quit unexpectedly"),
            Err(err) => err.context("checker failed"),
        }
    }
}
