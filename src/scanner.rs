//! Main loop: poll new blocks, find fresh PancakeSwap pairs, analyse, store, alert.
use crate::analysis::{TokenFacts, assess};
use crate::chain::{Chain, NewPair};
use crate::config::{AlertMode, Config};
use crate::store::Store;
use crate::telegram::{Telegram, format_alert};
use anyhow::Result;
use tracing::{info, warn};

pub struct Scanner {
    pub(crate) chain: Chain,
    pub(crate) store: Store,
    pub(crate) telegram: Option<Telegram>,
    pub(crate) config: Config,
}

/// Splits `[from, to]` into consecutive ranges of at most `max` blocks.
pub fn block_ranges(from: u64, to: u64, max: u64) -> Vec<(u64, u64)> {
    let max = max.max(1);
    let mut ranges = Vec::new();
    let mut start = from;
    while start <= to {
        let end = (start + max - 1).min(to);
        ranges.push((start, end));
        start = end + 1;
    }
    ranges
}

impl Scanner {
    pub fn new(chain: Chain, store: Store, config: Config) -> Result<Self> {
        let telegram = match config.telegram_credentials() {
            Some((token, chat_id)) => Some(Telegram::new(token, chat_id)?),
            None => None,
        };
        Ok(Self {
            chain,
            store,
            telegram,
            config,
        })
    }

    /// Runs forever: every `poll_interval`, scans the blocks produced since the last pass.
    pub async fn run(&self) -> Result<()> {
        if let Some(tg) = &self.telegram
            && let Err(err) = tg.register_commands().await
        {
            warn!(%err,"cannot register Telegram menu");
        }
        let commands = async {
            loop {
                if let Err(err) = self.poll_commands().await {
                    warn!(%err,"Telegram commands unavailable; retrying");
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        };
        tokio::try_join!(self.scan_loop(), commands)?;
        Ok(())
    }
    async fn scan_loop(&self) -> Result<()> {
        let head = self
            .chain
            .latest_block()
            .await?
            .saturating_sub(self.config.confirmations);
        let mut next = match self.store.last_block().await? {
            Some(last) => last + 1,
            None => head.saturating_sub(self.config.start_lookback),
        };
        info!(
            from = next,
            head,
            telegram = self.telegram.is_some(),
            "scanning new pairs"
        );

        loop {
            match self.chain.latest_block().await {
                Ok(head) => {
                    if let Err(err) = self
                        .store
                        .set_setting("chain_head", &head.to_string())
                        .await
                    {
                        warn!(%err,"cannot persist head metric");
                    }
                    next = self
                        .scan_until(next, head.saturating_sub(self.config.confirmations))
                        .await
                }
                Err(err) => warn!(%err, "cannot read the latest block"),
            }
            match self.store.due_pairs().await {
                Ok(pairs) => {
                    for pair in pairs {
                        if let Err(err) = self.handle(pair).await {
                            warn!(%err, token = %pair.token, "scheduled reanalysis failed");
                        }
                    }
                }
                Err(err) => warn!(%err, "cannot load scheduled analyses"),
            }
            if let Err(err) = self.deliver_alerts().await {
                warn!(%err, "alert delivery failed; retrying next pass");
            }
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Scans `[next, head]` range by range and returns the first block still to scan.
    /// Stops at the first failing range so it is retried on the next pass.
    async fn scan_until(&self, mut next: u64, head: u64) -> u64 {
        'ranges: for (from, to) in block_ranges(next, head, self.config.max_block_range) {
            let pairs = match self.chain.new_pairs(from, to).await {
                Ok(pairs) => pairs,
                Err(err) => {
                    warn!(from, to, %err, "eth_getLogs failed, retrying later");
                    break;
                }
            };
            for pair in pairs {
                if let Err(err) = self.handle(pair).await {
                    warn!(token = %pair.token, %err, "analysis failed; range will be retried");
                    break 'ranges;
                }
            }
            if let Err(err) = self.store.set_last_block(to).await {
                // Retry this range; already saved tokens are skipped.
                warn!(%err, "cannot save the scan cursor");
                break;
            }
            next = to + 1;
        }
        next
    }

    async fn deliver_alerts(&self) -> Result<()> {
        if self.store.setting("paused").await?.as_deref() == Some("true") {
            return Ok(());
        }
        if let Some(tg) = &self.telegram {
            for (id, message) in self.store.change_alerts().await? {
                tg.send(&message).await?;
                self.store.delivered_change(id).await?;
            }
            for (token, message) in self.store.pending_alerts().await? {
                tg.send(&message).await?;
                self.store.mark_delivered(&token).await?;
            }
        }
        Ok(())
    }

    /// Analyses one token. Public so the `check <address>` command can reuse it.
    pub async fn analyse(&self, pair: NewPair) -> Result<TokenFacts> {
        let chain = &self.chain;
        let (meta, ownership, bytecode, liquidity, simulation) = tokio::join!(
            chain.token_metadata(pair.token),
            chain.ownership(pair.token),
            chain.bytecode(pair.token),
            chain.liquidity(pair.pair, pair.token),
            chain.simulate(pair.token, self.config.sim_buy_value),
        );
        let (name, symbol, decimals) = meta;
        let bytecode = bytecode?;
        let (liquidity_bnb, lp_burned_ratio) = liquidity?;
        let capabilities = bytecode
            .capabilities
            .iter()
            .map(|c| c.label().to_owned())
            .collect();
        let evidence = if self
            .store
            .watches()
            .await?
            .contains(&pair.token.to_string())
        {
            tokio::time::timeout(
                std::time::Duration::from_secs(25),
                crate::enrichment::collect(&self.config.rpc_url, pair.token, pair.pair),
            )
            .await
            .unwrap_or_else(|_| serde_json::json!({"status":"timeout"}))
        } else {
            serde_json::json!({"status":"not_requested","hint":"Use /deep for detailed evidence"})
        };
        Ok(TokenFacts {
            evidence,
            token: pair.token,
            pair: pair.pair,
            name,
            symbol,
            decimals,
            liquidity_bnb,
            lp_burned_ratio,
            ownership,
            bytecode,
            capabilities,
            simulation,
        })
    }

    async fn handle(&self, pair: NewPair) -> Result<()> {
        let facts = self.analyse(pair).await?;
        let assessment = assess(&facts);
        info!(
            token = %facts.token,
            symbol = %facts.symbol,
            score = assessment.score,
            honeypot = assessment.honeypot,
            liquidity_bnb = format!("{:.2}", facts.liquidity_bnb),
            "analysed"
        );

        let min_score = self
            .store
            .setting("min_score")
            .await?
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(self.config.alert_min_score);
        let min_liquidity = self
            .store
            .setting("min_liquidity")
            .await?
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(self.config.alert_min_liquidity);
        let max_tax = self
            .store
            .setting("max_tax")
            .await?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(self.config.alert_max_tax_bps);
        let wanted = match self.config.alert_mode {
            AlertMode::All => true,
            AlertMode::MinScore => assessment.complete && assessment.score >= min_score,
            AlertMode::Off => false,
        };
        self.store
            .save(pair.block, &facts, &assessment, false)
            .await?;
        let passes_filters = facts.liquidity_bnb >= min_liquidity
            && [
                facts.simulation.buy_tax_bps,
                facts.simulation.sell_tax_bps,
                facts.simulation.transfer_tax_bps,
            ]
            .into_iter()
            .flatten()
            .all(|tax| tax <= max_tax);
        if wanted && passes_filters && self.telegram.is_some() {
            self.store
                .queue_alert(&pair.token.to_string(), &format_alert(&facts, &assessment))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::block_ranges;

    #[test]
    fn ranges_cover_the_interval_without_overlap() {
        assert_eq!(block_ranges(10, 10, 500), vec![(10, 10)]);
        assert_eq!(block_ranges(1, 1000, 500), vec![(1, 500), (501, 1000)]);
        assert_eq!(
            block_ranges(1, 1001, 500),
            vec![(1, 500), (501, 1000), (1001, 1001)]
        );
        assert!(block_ranges(5, 4, 500).is_empty());
        assert_eq!(block_ranges(1, 3, 0), vec![(1, 1), (2, 2), (3, 3)]);
    }
}
