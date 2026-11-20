//! Private-chat commands, restricted to the configured owner.
use crate::{analysis::assess, chain::NewPair, scanner::Scanner, telegram::format_alert};
use alloy::primitives::Address;
use anyhow::{Context, Result};

const HELP: &str = "🤖 <b>RugRadar</b>\n/check adresse — analyse immédiate\n/watch adresse — suivre un token\n/unwatch adresse — retirer du suivi\n/watchlist — liste suivie\n/stats — statistiques\n/pause — suspendre les alertes\n/resume — reprendre les alertes\n/help — cette aide";

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
            if tg.authorized(message) {
                if let Some(text) = message["text"].as_str() {
                    let reply = match self.command(text).await {
                        Ok(reply) => reply,
                        Err(_) => "Impossible de traiter la demande. Vérifie l'adresse et réessaie ; le RPC peut être indisponible.".into(),
                    };
                    tg.send(&reply).await?;
                }
            }
            self.store
                .set_setting("telegram_offset", &(id + 1).to_string())
                .await?;
        }
        Ok(())
    }

    async fn command(&self, text: &str) -> Result<String> {
        let mut words = text.split_whitespace();
        let command = words.next().unwrap_or("").split('@').next().unwrap_or("");
        match command {
            "/start" | "/help" => Ok(HELP.into()),
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
                let token: Address = words.next().context("address required")?.parse()?;
                if command == "/unwatch" {
                    self.store.watch(&token.to_string(), false).await?;
                    return Ok("Token retiré du suivi.".into());
                }
                let pair = self.chain.pair_for(token).await?.context("no WBNB pair")?;
                let block = self.chain.latest_block().await?;
                let facts = self.analyse(NewPair { token, pair, block }).await?;
                let assessment = assess(&facts);
                if command == "/watch" {
                    self.store.save(block, &facts, &assessment, false).await?;
                    self.store.watch(&token.to_string(), true).await?;
                }
                Ok(format_alert(&facts, &assessment))
            }
            _ => Ok(HELP.into()),
        }
    }
}
