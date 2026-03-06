use std::{path::PathBuf, sync::Arc};

use clap::Parser;
use lib_btcmap_proxy::{btcmap::BtcMapClient, config::{Config, EnvOverride}, server};

#[derive(Parser)]
#[clap(long_about = None)]
struct Cli {
    #[clap(
        short,
        long,
        env = "BTCMAP_PROXY_CONFIG",
        default_value = "btcmap-proxy.yml",
        value_name = "FILE"
    )]
    config: PathBuf,
    #[clap(env = "BTCMAP_API_KEY")]
    btcmap_api_key: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config = Config::from_path(cli.config, EnvOverride { btcmap_api_key: cli.btcmap_api_key.clone() })?;

    tracing::init_tracer(config.tracing.clone())?;

    let btcmap = Arc::new(BtcMapClient::new(
        config.app.btcmap_api_url.clone(),
        cli.btcmap_api_key,
        config.app.btcmap_origin.clone(),
    ));

    server::run_server(config.server, btcmap).await
}
