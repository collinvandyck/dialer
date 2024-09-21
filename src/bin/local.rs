use std::path::PathBuf;

use clap::Parser;
use dialer::config::Config;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(clap::Parser)]
struct Args {
    #[arg(long, default_value = "checks.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
    let filter_layer = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(tracing_error::ErrorLayer::default())
        .init();
    let args = Args::parse();
    let config = Config::from_path(&args.config).await?;
    let checker = dialer::check::Checker::from_config(&config).await?;
    checker.run().await?;
    Ok(())
}
