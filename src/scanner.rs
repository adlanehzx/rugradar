//! Main loop: poll new blocks, find fresh PancakeSwap pairs, analyse, store, alert.
use crate::analysis::{TokenFacts, assess};
use crate::chain::{Chain, NewPair};
use crate::config::{AlertMode, Config};
use crate::store::Store;
use crate::telegram::{Telegram, format_alert};
use anyhow::Result;
use tracing::{info, warn};

pub struct Scanner {
    chain: Chain,
    store: Store,
    telegram: Option<Telegram>,
    config: Config,
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
        let head = self.chain.latest_block().await?;
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
                Ok(head) => next = self.scan_until(next, head).await,
                Err(err) => warn!(%err, "cannot read the latest block"),
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
        Ok(TokenFacts {
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
        if self.store.is_known(&pair.token.to_string()).await? {
            return Ok(());
        }
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

        let wanted = match self.config.alert_mode {
            AlertMode::All => true,
            AlertMode::MinScore => assessment.score >= self.config.alert_min_score,
            AlertMode::Off => false,
        };
        let mut alerted = false;
        if let (true, Some(tg)) = (wanted, &self.telegram) {
            match tg.send(&format_alert(&facts, &assessment)).await {
                Ok(()) => alerted = true,
                Err(err) => warn!(%err, "telegram alert failed"),
            }
        }
        self.store
            .save(pair.block, &facts, &assessment, alerted)
            .await
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
