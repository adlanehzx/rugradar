//! Private-chat commands, restricted to the configured owner.
use crate::{analysis::assess, chain::NewPair, scanner::Scanner, telegram::format_alert};
use alloy::primitives::Address;
use anyhow::{Context, Result};

const HELP: &str = "🤖 <b>RugRadar</b>\n/check adresse — analyse immédiate\n/watch adresse — suivre un token\n/unwatch adresse — retirer du suivi\n/watchlist — liste suivie\n/stats — statistiques\n/pause — suspendre les alertes\n/resume — reprendre les alertes\n/deep adresse — détenteurs, rôles, proxy\n/filters — régler les filtres\n/help — cette aide";

impl Scanner {
    pub(crate) async fn poll_commands(&self) -> Result<()> {
        let Some(tg) = &self.telegram else {
            return Ok(());
        };
        let offset = self
            .store
            .setting("telegram_offset")
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        for update in tg.updates(offset).await? {
            let id = update["update_id"].as_i64().context("missing update id")?;
            let message = &update["message"];
            if tg.authorized(message)
                && let Some(text) = message["text"].as_str()
            {
                let reply = match self.command(text).await {
                        Ok(reply) => reply,
                        Err(_) => "Impossible de traiter la demande. Vérifie l'adresse et réessaie ; le RPC peut être indisponible.".into(),
                    };
                tg.send(&reply).await?;
            }
            self.store
                .set_setting("telegram_offset", &(id + 1).to_string())
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn command(&self, text: &str) -> Result<String> {
        let mut words = text.split_whitespace();
        let command = words.next().unwrap_or("").split('@').next().unwrap_or("");
        match command {
            "/start" | "/help" => Ok(HELP.into()),
            "/filters" => {
                let key = words.next();
                if let Some(key) = key {
                    let Some(value) = words.next() else {
                        return Ok("Usage : /filters min_score 60 | min_liquidity 1 | max_tax 1000 (basis points)".into());
                    };
                    if !valid_filter(key, value) {
                        return Ok("Filtre invalide : min_score 0–100, min_liquidity ≥ 0, max_tax 0–10000 bps.".into());
                    }
                    self.store.set_setting(key, value).await?;
                    return Ok("✅ Filtre enregistré.".into());
                }
                Ok(format!(
                    "Filtres :\nmin_score : {}\nmin_liquidity : {} BNB\nmax_tax : {} bps\nExemple : /filters min_score 75",
                    self.store
                        .setting("min_score")
                        .await?
                        .unwrap_or(self.config.alert_min_score.to_string()),
                    self.store
                        .setting("min_liquidity")
                        .await?
                        .unwrap_or(self.config.alert_min_liquidity.to_string()),
                    self.store
                        .setting("max_tax")
                        .await?
                        .unwrap_or(self.config.alert_max_tax_bps.to_string())
                ))
            }
            "/deep" => {
                let Some(token) = words.next().and_then(|s| s.parse::<Address>().ok()) else {
                    return Ok("Usage : /deep 0xadresse".into());
                };
                let Some(pair) = self.chain.pair_for(token).await? else {
                    return Ok("Aucune paire WBNB sur ce DEX.".into());
                };
                let evidence = tokio::time::timeout(
                    std::time::Duration::from_secs(45),
                    crate::enrichment::collect(&self.config.rpc_url, token, pair),
                )
                .await;
                Ok(match evidence {
                    Ok(e) => crate::telegram::escape(&serde_json::to_string_pretty(&e)?),
                    Err(_) => "Analyse approfondie expirée. Réessaie plus tard.".into(),
                })
            }
            "/stats" => {
                let (total, honeypots) = self.store.stats().await?;
                Ok(format!(
                    "📊 {total} tokens analysés, {honeypots} honeypots détectés.\nAlertes suspendues : {}",
                    self.store.setting("paused").await?.as_deref() == Some("true")
                ))
            }
            "/pause" | "/resume" => {
                self.store
                    .set_setting("paused", if command == "/pause" { "true" } else { "false" })
                    .await?;
                Ok(if command == "/pause" {
                    "⏸ Alertes suspendues. Le scanner continue."
                } else {
                    "▶️ Alertes reprises."
                }
                .into())
            }
            "/watchlist" => {
                let tokens = self.store.watches().await?;
                Ok(if tokens.is_empty() {
                    "Liste de suivi vide.".into()
                } else {
                    tokens.join("\n")
                })
            }
            "/check" | "/watch" | "/unwatch" => {
                let Some(token) = words.next().and_then(|s| s.parse::<Address>().ok()) else {
                    return Ok(
                        "Adresse invalide. Exemple : /check 0x… (40 caractères hexadécimaux)."
                            .into(),
                    );
                };
                if command == "/unwatch" {
                    self.store.watch(&token.to_string(), false).await?;
                    return Ok("Token retiré du suivi.".into());
                }
                let Some(pair) = self.chain.pair_for(token).await? else {
                    return Ok("Aucune paire WBNB trouvée sur PancakeSwap V2.".into());
                };
                let block = self.chain.latest_block().await?;
                let facts = self.analyse(NewPair { token, pair, block }).await?;
                let assessment = assess(&facts);
                if command == "/watch" {
                    self.store.save(block, &facts, &assessment, false).await?;
                    self.store.watch(&token.to_string(), true).await?;
                }
                let report = format_alert(&facts, &assessment);
                Ok(if command == "/watch" {
                    format!("✅ Token ajouté au suivi.\n{report}")
                } else {
                    report
                })
            }
            _ => Ok(HELP.into()),
        }
    }
}

fn valid_filter(key: &str, value: &str) -> bool {
    match key {
        "min_score" => value.parse::<u8>().is_ok_and(|v| v <= 100),
        "max_tax" => value.parse::<u32>().is_ok_and(|v| v <= 10000),
        "min_liquidity" => value
            .parse::<f64>()
            .is_ok_and(|v| v.is_finite() && v >= 0.0),
        _ => false,
    }
}
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn commands_persist_pause_filters_and_reject_invalid_address() {
        use crate::{
            chain::Chain,
            config::{AlertMode, Config, PANCAKE_V2_FACTORY, PANCAKE_V2_ROUTER, WBNB},
            scanner::Scanner,
            store::Store,
        };
        let config = Config {
            rpc_url: "http://127.0.0.1:1".into(),
            chain_id: 56,
            factory: PANCAKE_V2_FACTORY,
            router: PANCAKE_V2_ROUTER,
            wrapped_native: WBNB,
            database_url: "sqlite::memory:".into(),
            poll_interval: std::time::Duration::from_secs(1),
            start_lookback: 100,
            confirmations: 0,
            max_block_range: 100,
            sim_buy_value: alloy::primitives::U256::from(1),
            telegram_bot_token: None,
            telegram_chat_id: None,
            alert_mode: AlertMode::Off,
            alert_min_score: 60,
            alert_min_liquidity: 0.0,
            alert_max_tax_bps: 10000,
        };
        let store = Store::open("sqlite::memory:").await.unwrap();
        let scanner =
            Scanner::new(Chain::connect(&config).unwrap(), store.clone(), config).unwrap();
        scanner.command("/pause").await.unwrap();
        assert_eq!(
            store.setting("paused").await.unwrap().as_deref(),
            Some("true")
        );
        scanner.command("/filters min_score 75").await.unwrap();
        assert_eq!(
            store.setting("min_score").await.unwrap().as_deref(),
            Some("75")
        );
        scanner.command("/filters min_score 999").await.unwrap();
        assert_eq!(
            store.setting("min_score").await.unwrap().as_deref(),
            Some("75")
        );
        assert!(
            scanner
                .command("/check invalid")
                .await
                .unwrap()
                .contains("invalide")
        );
        scanner.command("/resume").await.unwrap();
        assert_eq!(
            store.setting("paused").await.unwrap().as_deref(),
            Some("false")
        );
    }

    #[test]
    fn filter_validation_rejects_nonfinite_and_out_of_range() {
        for (key, value) in [
            ("min_score", "101"),
            ("max_tax", "10001"),
            ("min_liquidity", "NaN"),
            ("min_liquidity", "-1"),
            ("unexpected", "1"),
        ] {
            assert!(!super::valid_filter(key, value));
        }
        assert!(super::valid_filter("min_score", "75"));
        assert!(super::valid_filter("max_tax", "1000"));
    }
}
