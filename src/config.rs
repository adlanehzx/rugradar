//! Runtime configuration, read from environment variables (see `.env.example`).
use alloy::primitives::{Address, U256, address, utils::parse_ether};
use anyhow::{Context, Result};
use std::{env, str::FromStr, time::Duration};

/// PancakeSwap V2 on BNB Smart Chain (defaults; any Uniswap V2 fork can be configured).
pub const PANCAKE_V2_FACTORY: Address = address!("cA143Ce32Fe78f1f7019d7d551a6402fC5350c73");
pub const PANCAKE_V2_ROUTER: Address = address!("10ED43C718714eb63d5aA57B78B54704E256024E");
pub const WBNB: Address = address!("bb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c");

/// Which analysed tokens trigger a Telegram alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertMode {
    /// Every analysed token.
    All,
    /// Only tokens whose score is at least `alert_min_score`.
    MinScore,
    /// No alerts (log and store only).
    Off,
}

impl FromStr for AlertMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "min_score" | "minscore" | "safe" => Ok(Self::MinScore),
            "off" | "none" => Ok(Self::Off),
            other => anyhow::bail!("unknown ALERT_MODE `{other}` (expected all, min_score or off)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_url: String,
    pub chain_id: u64,
    pub factory: Address,
    pub router: Address,
    pub wrapped_native: Address,
    pub database_url: String,
    pub poll_interval: Duration,
    /// How many blocks back to scan on start-up when the database is empty.
    pub start_lookback: u64,
    pub confirmations: u64,
    /// Maximum block range requested per `eth_getLogs` call.
    pub max_block_range: u64,
    /// BNB spent in the simulated buy (wei).
    pub sim_buy_value: U256,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub alert_mode: AlertMode,
    pub alert_min_score: u8,
}

fn var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn parse<T: FromStr>(key: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    match var(key) {
        Some(raw) => raw
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid {key}: {e}")),
        None => Ok(default),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let sim_buy_value = parse_ether(&var("SIM_BUY_BNB").unwrap_or_else(|| "0.01".into()))
            .context("invalid SIM_BUY_BNB")?;
        anyhow::ensure!(
            !sim_buy_value.is_zero() && sim_buy_value <= parse_ether("10")?,
            "SIM_BUY_BNB must be in (0, 10]"
        );
        let alert_min_score: u8 = parse("ALERT_MIN_SCORE", 60)?;
        anyhow::ensure!(
            alert_min_score <= 100,
            "ALERT_MIN_SCORE must be between 0 and 100"
        );
        Ok(Self {
            rpc_url: var("RPC_URL")
                .context("RPC_URL is required (a BNB Smart Chain HTTP endpoint)")?,
            chain_id: parse("CHAIN_ID", 56)?,
            factory: parse("FACTORY_ADDRESS", PANCAKE_V2_FACTORY)?,
            router: parse("ROUTER_ADDRESS", PANCAKE_V2_ROUTER)?,
            wrapped_native: parse("WRAPPED_NATIVE_ADDRESS", WBNB)?,
            database_url: var("DATABASE_URL").unwrap_or_else(|| "sqlite://rugradar.db".into()),
            poll_interval: Duration::from_secs(parse("POLL_INTERVAL_SECS", 6)?),
            confirmations: parse("CONFIRMATIONS", 15)?,
            start_lookback: parse("START_LOOKBACK_BLOCKS", 200)?,
            max_block_range: parse("MAX_BLOCK_RANGE", 500)?,
            sim_buy_value,
            telegram_bot_token: var("TELEGRAM_BOT_TOKEN"),
            telegram_chat_id: var("TELEGRAM_CHAT_ID"),
            alert_mode: parse("ALERT_MODE", AlertMode::MinScore)?,
            alert_min_score,
        })
    }

    /// Bot token and chat id, when alerts are enabled and fully configured.
    pub fn telegram_credentials(&self) -> Option<(String, String)> {
        if self.alert_mode == AlertMode::Off {
            return None;
        }
        Some((
            self.telegram_bot_token.clone()?,
            self.telegram_chat_id.clone()?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_mode_parses_aliases() {
        assert_eq!("ALL".parse::<AlertMode>().unwrap(), AlertMode::All);
        assert_eq!("safe".parse::<AlertMode>().unwrap(), AlertMode::MinScore);
        assert_eq!("off".parse::<AlertMode>().unwrap(), AlertMode::Off);
        assert!("loud".parse::<AlertMode>().is_err());
    }
}
