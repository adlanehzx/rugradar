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

    #[cfg(test)]
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
            "INSERT INTO tokens
               (token, pair, block, name, symbol, score, level, honeypot, liquidity_bnb, report, alerted, next_check)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, unixepoch() + 300)
             ON CONFLICT(token) DO UPDATE SET name=excluded.name, symbol=excluded.symbol,
             score=excluded.score, level=excluded.level, honeypot=excluded.honeypot,
             liquidity_bnb=excluded.liquidity_bnb, report=excluded.report,
             analysed_at=datetime('now'), next_check=unixepoch() + 300",
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

    pub async fn due_pairs(&self) -> Result<Vec<crate::chain::NewPair>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT token, pair, block FROM tokens WHERE next_check <= unixepoch() ORDER BY next_check LIMIT 100"
        ).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|(token, pair, block)| {
                Ok(crate::chain::NewPair {
                    token: token.parse()?,
                    pair: pair.parse()?,
                    block: block as u64,
                })
            })
            .collect()
    }

    pub async fn queue_alert(&self, token: &str, message: &str) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO alert_outbox(token, message) VALUES (?, ?)")
            .bind(token)
            .bind(message)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn pending_alerts(&self) -> Result<Vec<(String, String)>> {
        Ok(
            sqlx::query_as("SELECT token, message FROM alert_outbox WHERE delivered = 0 LIMIT 100")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn mark_delivered(&self, token: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE alert_outbox SET delivered = 1 WHERE token = ?")
            .bind(token)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE tokens SET alerted = 1 WHERE token = ?")
            .bind(token)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn change_alerts(&self) -> Result<Vec<(i64, String)>> {
        Ok(sqlx::query_as(
            "SELECT id,message FROM change_alerts WHERE delivered=0 ORDER BY id LIMIT 50",
        )
        .fetch_all(&self.pool)
        .await?)
    }
    pub async fn delivered_change(&self, id: i64) -> Result<()> {
        sqlx::query("UPDATE change_alerts SET delivered=1 WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT value FROM settings WHERE key=?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?)
    }
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query("INSERT INTO settings(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(key).bind(value).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn watch(&self, token: &str, enabled: bool) -> Result<()> {
        let sql = if enabled {
            "INSERT OR IGNORE INTO watchlist(token) VALUES (?)"
        } else {
            "DELETE FROM watchlist WHERE token=?"
        };
        sqlx::query(sql).bind(token).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn watches(&self) -> Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT token FROM watchlist ORDER BY added_at LIMIT 100")
                .fetch_all(&self.pool)
                .await?,
        )
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
    async fn outbox_survives_reanalysis_and_reopen() {
        let path = std::env::temp_dir().join(format!("rugradar-outbox-{}.db", std::process::id()));
        let url = format!("sqlite://{}", path.display());
        let store = Store::open(&url).await.unwrap();
        let f = facts();
        let token = f.token.to_string();
        store.save(43, &f, &assess(&f), false).await.unwrap();
        store.queue_alert(&token, "first").await.unwrap();
        store.save(43, &f, &assess(&f), false).await.unwrap();
        store.queue_alert(&token, "duplicate").await.unwrap();
        assert_eq!(
            store.pending_alerts().await.unwrap(),
            vec![(token.clone(), "first".into())]
        );
        store.pool.close().await;
        let reopened = Store::open(&url).await.unwrap();
        assert_eq!(reopened.pending_alerts().await.unwrap().len(), 1);
        reopened.mark_delivered(&token).await.unwrap();
        assert!(reopened.pending_alerts().await.unwrap().is_empty());
        reopened.pool.close().await;
        std::fs::remove_file(path).unwrap();
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
