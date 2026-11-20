//! RugRadar — real-time risk scanner for new PancakeSwap V2 tokens on BNB Smart Chain.
mod abi;
mod analysis;
mod bytecode;
mod chain;
mod commands;
mod config;
mod enrichment;
mod scanner;
mod store;
mod telegram;

use alloy::primitives::Address;
use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage:
  rugradar                  watch new PancakeSwap V2 pairs (default)
  rugradar check <token>    analyse one token listed against WBNB
  rugradar stats            tokens analysed and honeypots found so far";

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = config::Config::from_env()?;
    let store = store::Store::open(&config.database_url).await?;
    if args.first().map(String::as_str) == Some("stats") {
        let (total, honeypots) = store.stats().await?;
        println!("{total} tokens analysed, {honeypots} honeypots detected");
        return Ok(());
    }

    let chain = chain::Chain::connect(&config)?;
    let chain_id = chain.chain_id().await.context("cannot reach the RPC")?;
    anyhow::ensure!(
        chain_id == config.chain_id,
        "RPC_URL points to chain {chain_id}, expected {} (set CHAIN_ID to override)",
        config.chain_id
    );
    let scanner = scanner::Scanner::new(chain.clone(), store, config)?;

    match args.first().map(String::as_str) {
        None | Some("watch") => scanner.run().await,
        Some("check") => {
            let token: Address = args
                .get(1)
                .context(USAGE)?
                .parse()
                .context("invalid token address")?;
            let pair = chain
                .pair_for(token)
                .await?
                .context("no PancakeSwap V2 pair against WBNB")?;
            let facts = scanner
                .analyse(chain::NewPair {
                    token,
                    pair,
                    block: 0,
                })
                .await?;
            let assessment = analysis::assess(&facts);
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({ "facts": facts, "assessment": assessment })
                )?
            );
            Ok(())
        }
        Some(_) => {
            eprintln!("{USAGE}");
            std::process::exit(2)
        }
    }
}
