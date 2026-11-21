//! Telegram alerts through the Bot API (`sendMessage`, HTML formatting).
use crate::analysis::{Assessment, TokenFacts};
use anyhow::Result;

pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn tax(bps: Option<u32>) -> String {
    bps.map(|b| format!("{:.1} %", b as f64 / 100.0))
        .unwrap_or_else(|| "?".into())
}

/// Builds the alert text. Kept separate from sending so it can be unit-tested.
pub fn format_alert(f: &TokenFacts, a: &Assessment) -> String {
    let mut msg = format!(
        "<b>{}</b> ({}) — indice heuristique <b>{}/100</b>\n{}\n\n",
        escape(&f.name),
        escape(&f.symbol),
        a.score,
        a.level.label()
    );
    if !f.simulation.trials.is_empty() {
        msg.push_str(&format!(
            "Scénarios : {} · bloc {} · taxes maximales observées\n",
            f.simulation.trials.len(),
            f.simulation.block.unwrap_or_default()
        ));
    }
    if !a.simulation_complete {
        msg.push_str("⚪ Analyse incomplète : revente non vérifiée\n");
    }
    msg.push_str(&format!(
        "💧 Liquidité : {:.2} BNB · LP brûlés : {:.0} %\n💸 Taxes : achat {} · vente {} · transfert {}\n",
        f.liquidity_bnb,
        f.lp_burned_ratio * 100.0,
        tax(f.simulation.buy_tax_bps),
        tax(f.simulation.sell_tax_bps),
        tax(f.simulation.transfer_tax_bps),
    ));
    if a.findings.is_empty() {
        msg.push_str("\nAucun signal détecté par les contrôles exécutés\n");
    } else {
        msg.push('\n');
        for finding in &a.findings {
            msg.push_str(&format!("• {}\n", escape(finding)));
        }
    }
    msg.push_str("\n⚪ Couverture partielle : permissions, détenteurs et lockers non validés exhaustivement. Cet indice n'est pas une probabilité de sécurité.\n");
    msg.push_str(&format!(
        "\n<a href=\"https://bscscan.com/token/{token}\">BscScan</a> · \
         <a href=\"https://dexscreener.com/bsc/{pair}\">DexScreener</a>\n<code>{token}</code>",
        token = f.token,
        pair = f.pair,
    ));
    msg
}

#[derive(Clone)]
pub struct Telegram {
    client: reqwest::Client,
    token: String,
    chat_id: String,
}

impl Telegram {
    pub fn new(token: String, chat_id: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            token,
            chat_id,
        })
    }

    pub async fn register_commands(&self) -> Result<()> {
        let commands = [
            ("help", "Aide"),
            ("check", "Analyser un token"),
            ("deep", "Examiner détenteurs rôles et proxy"),
            ("watch", "Suivre un token"),
            ("unwatch", "Retirer du suivi"),
            ("watchlist", "Liste suivie"),
            ("filters", "Réglages des alertes"),
            ("stats", "Statistiques"),
            ("pause", "Suspendre les alertes"),
            ("resume", "Reprendre les alertes"),
        ];
        let response=self.client.post(format!("https://api.telegram.org/bot{}/setMyCommands",self.token))
            .json(&serde_json::json!({"commands":commands.iter().map(|(command,description)|serde_json::json!({"command":command,"description":description})).collect::<Vec<_>>(),"scope":{"type":"chat","chat_id":self.chat_id}}))
            .send().await.map_err(|_|anyhow::anyhow!("Telegram menu unavailable"))?;
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Invalid Telegram menu response"))?;
        anyhow::ensure!(body["ok"] == true, "Telegram menu rejected");
        Ok(())
    }
    pub async fn updates(&self, offset: i64) -> Result<Vec<serde_json::Value>> {
        let response = self.client.post(format!("https://api.telegram.org/bot{}/getUpdates", self.token))
            .json(&serde_json::json!({"offset":offset,"timeout":0,"allowed_updates":["message"],"limit":20}))
            .send().await.map_err(|_| anyhow::anyhow!("Telegram polling transport error"))?;
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Invalid Telegram polling response"))?;
        anyhow::ensure!(body["ok"] == true, "Telegram polling rejected");
        Ok(body["result"].as_array().cloned().unwrap_or_default())
    }
    pub fn authorized(&self, message: &serde_json::Value) -> bool {
        message["chat"]["type"] == "private"
            && message["chat"]["id"]
                .as_i64()
                .is_some_and(|id| id.to_string() == self.chat_id)
            && message["from"]["id"] == message["chat"]["id"]
    }

    pub async fn send(&self, text: &str) -> Result<()> {
        if text.encode_utf16().count() > 3500 {
            let chars: Vec<char> = text.chars().collect();
            for chunk in chars.chunks(1000) {
                self.send_part(&chunk.iter().collect::<String>(), false)
                    .await?;
            }
            return Ok(());
        }
        self.send_part(text, true).await
    }
    async fn send_part(&self, text: &str, html: bool) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": if html { "HTML" } else { "" },
            "disable_web_page_preview": true,
            "reply_markup": {"keyboard":[[{"text":"/stats"},{"text":"/watchlist"}],[{"text":"/filters"},{"text":"/help"}],[{"text":"/pause"},{"text":"/resume"}]],"resize_keyboard":true},
        });
        let res = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Telegram request failed"))?;
        anyhow::ensure!(
            res.status().is_success(),
            "Telegram returned {}",
            res.status()
        );
        let body: serde_json::Value = res
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Invalid Telegram response"))?;
        anyhow::ensure!(body["ok"] == true, "Telegram message rejected");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Ownership, SimOutcome, Simulation, assess};
    use crate::bytecode::BytecodeReport;
    use alloy::primitives::Address;

    #[test]
    fn only_configured_private_owner_can_run_commands() {
        let tg = Telegram::new("dummy".into(), "123".into()).unwrap();
        assert!(tg.authorized(
            &serde_json::json!({"chat":{"id":123,"type":"private"},"from":{"id":123}})
        ));
        assert!(!tg.authorized(
            &serde_json::json!({"chat":{"id":456,"type":"private"},"from":{"id":456}})
        ));
        assert!(
            !tg.authorized(
                &serde_json::json!({"chat":{"id":123,"type":"group"},"from":{"id":123}})
            )
        );
        assert!(!tg.authorized(
            &serde_json::json!({"chat":{"id":123,"type":"private"},"from":{"id":456}})
        ));
    }

    #[test]
    fn alert_contains_key_information_and_escapes_html() {
        let f = TokenFacts {
            evidence: serde_json::Value::Null,
            token: Address::repeat_byte(0x11),
            pair: Address::repeat_byte(0x22),
            name: "<Moon> & Co".into(),
            symbol: "MOON".into(),
            decimals: 9,
            liquidity_bnb: 0.5,
            lp_burned_ratio: 0.0,
            ownership: Ownership::Owned(Address::repeat_byte(0x33)),
            bytecode: BytecodeReport::default(),
            capabilities: vec![],
            simulation: Simulation {
                trials: vec![],
                block: None,
                outcome: SimOutcome::Ok,
                buy_tax_bps: Some(500),
                sell_tax_bps: Some(1_200),
                transfer_tax_bps: Some(0),
            },
        };
        let text = format_alert(&f, &assess(&f));
        assert!(text.contains("&lt;Moon&gt; &amp; Co"));
        assert!(text.contains("vente 12.0 %"));
        assert!(text.contains("liquidité faible"));
        assert!(text.contains("bscscan.com/token/0x1111111111111111111111111111111111111111"));
    }
}
