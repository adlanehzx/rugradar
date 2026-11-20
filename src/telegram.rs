//! Telegram alerts through the Bot API (`sendMessage`, HTML formatting).
use crate::analysis::{Assessment, TokenFacts};
use anyhow::{Context, Result};

fn escape(text: &str) -> String {
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
        "<b>{}</b> ({}) — score <b>{}/100</b>\n{}\n\n",
        escape(&f.name),
        escape(&f.symbol),
        a.score,
        a.level.label()
    );
    msg.push_str(&format!(
        "💧 Liquidité : {:.2} BNB · LP brûlés : {:.0} %\n💸 Taxes : achat {} · vente {} · transfert {}\n",
        f.liquidity_bnb,
        f.lp_burned_ratio * 100.0,
        tax(f.simulation.buy_tax_bps),
        tax(f.simulation.sell_tax_bps),
        tax(f.simulation.transfer_tax_bps),
    ));
    if a.findings.is_empty() {
        msg.push_str("\n✅ Aucun signal d'alerte détecté\n");
    } else {
        msg.push('\n');
        for finding in &a.findings {
            msg.push_str(&format!("• {}\n", escape(finding)));
        }
    }
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

    pub async fn send(&self, text: &str) -> Result<()> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        let res = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .context("Telegram request failed")?;
        anyhow::ensure!(
            res.status().is_success(),
            "Telegram returned {}",
            res.status()
        );
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
    fn alert_contains_key_information_and_escapes_html() {
        let f = TokenFacts {
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
