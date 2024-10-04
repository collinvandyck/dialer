use crate::{
    api::Api,
    check::{self, Checker},
    config,
    db::Db,
};
use anyhow::Result;

#[derive(Clone)]
pub struct App {
    db: Db,
    api: Api,
    checker: check::Checker,
}

impl App {
    pub async fn new(config: &config::Config) -> Result<Self> {
        let db = Db::connect(&config.db_path).await?;
        let checker = Checker::new(db.clone(), config).await?;
        let api = Api::new(db.clone())?;
        Ok(Self { db, api, checker })
    }

    pub async fn run(&self) -> Result<()> {
        let app = self.clone();
        self.checker.run().await?;
        Ok(())
    }
}
