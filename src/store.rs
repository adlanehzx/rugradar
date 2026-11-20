//! SQLite persistence: analysed tokens and the last scanned block (so restarts resume).
use crate::analysis::{Assessment, TokenFacts};
use anyhow::Result;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::str::FromStr;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub async fn open(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn last_block(&self) -> Result<Option<u64>> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT last_block FROM cursor WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(b,)| b as u64))
    }

    pub async fn set_last_block(&self, block: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO cursor (id, last_block) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET last_block = excluded.last_block",
        )
        .bind(block as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn is_known(&self, token: &str) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM tokens WHERE token = ?1")
            .bind(token)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn save(
        &self,
        block: u64,
        facts: &TokenFacts,
        a: &Assessment,
        alerted: bool,
    ) -> Result<()> {
        let report = serde_json::json!({ "facts": facts, "assessment": a }).to_string();
        sqlx::query(
            "INSERT OR REPLACE INTO tokens
               (token, pair, block, name, symbol, score, level, honeypot, liquidity_bnb, report, alerted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(facts.token.to_string())
        .bind(facts.pair.to_string())
        .bind(block as i64)
        .bind(&facts.name)
        .bind(&facts.symbol)
        .bind(a.score as i64)
        .bind(format!("{:?}", a.level))
        .bind(a.honeypot)
        .bind(facts.liquidity_bnb)
        .bind(report)
        .bind(alerted)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn stats(&self) -> Result<(i64, i64)> {
        let row: (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(honeypot), 0) FROM tokens")
                .fetch_one(&self.pool)
                .await?;
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Ownership, SimOutcome, Simulation, assess};
    use crate::bytecode::BytecodeReport;
    use alloy::primitives::Address;

    fn facts() -> TokenFacts {
        TokenFacts {
            token: Address::repeat_byte(0xaa),
            pair: Address::repeat_byte(0xbb),
            name: "Test".into(),
            symbol: "TST".into(),
            decimals: 18,
            liquidity_bnb: 3.0,
            lp_burned_ratio: 1.0,
            ownership: Ownership::Renounced,
            bytecode: BytecodeReport::default(),
            capabilities: vec![],
            simulation: Simulation {
                outcome: SimOutcome::SellFailed,
                buy_tax_bps: None,
                sell_tax_bps: None,
                transfer_tax_bps: None,
            },
        }
    }

    #[tokio::test]
    async fn persists_tokens_and_cursor() {
        let store = Store::open("sqlite::memory:").await.unwrap();
        assert_eq!(store.last_block().await.unwrap(), None);
        store.set_last_block(42).await.unwrap();
        store.set_last_block(43).await.unwrap();
        assert_eq!(store.last_block().await.unwrap(), Some(43));

        let f = facts();
        let token = f.token.to_string();
        assert!(!store.is_known(&token).await.unwrap());
        store.save(43, &f, &assess(&f), false).await.unwrap();
        assert!(store.is_known(&token).await.unwrap());
        assert_eq!(store.stats().await.unwrap(), (1, 1));
    }
}
