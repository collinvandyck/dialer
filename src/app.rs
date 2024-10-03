use crate::{
    check::{self, Checker},
    config,
    db::Db,
};
use anyhow::Result;

#[derive(Clone)]
pub struct App {
    db: Db,
    checker: check::Checker,
}

impl App {
    pub async fn new(config: &config::Config) -> Result<Self> {
        let db = Db::connect(&config.db_path).await?;
        let checker = Checker::new(db.clone(), config).await?;
        Ok(Self { db, checker })
    }

    pub async fn run(&self) -> Result<()> {
        self.checker.run().await?;
        Ok(())
    }
}
