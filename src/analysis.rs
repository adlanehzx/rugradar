//! Turns raw on-chain facts about a token into a risk score and human-readable findings.
//! Everything here is pure, so the scoring rules are fully unit-tested.
use crate::bytecode::{BytecodeReport, Capability};
use alloy::primitives::{Address, U256};
use serde::Serialize;

/// Outcome of the buy → transfer → sell simulation (mirrors `Simulator.sol` status codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SimOutcome {
    Ok,
    NoRoute,
    BuyFailed,
    ApproveFailed,
    SellFailed,
    NothingReceived,
    TransferFailed,
    /// The RPC rejected the call (e.g. no state-override support).
    Unavailable,
}

impl SimOutcome {
    pub fn from_status(status: u8) -> Self {
        match status {
            0 => Self::Ok,
            1 => Self::NoRoute,
            2 => Self::BuyFailed,
            3 => Self::ApproveFailed,
            4 => Self::SellFailed,
            5 => Self::NothingReceived,
            6 => Self::TransferFailed,
            _ => Self::Unavailable,
        }
    }

    /// The token can be bought but not sold or moved: a honeypot.
    pub fn is_honeypot(self) -> bool {
        matches!(
            self,
            Self::ApproveFailed | Self::SellFailed | Self::NothingReceived | Self::TransferFailed
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Simulation {
    pub outcome: SimOutcome,
    /// Taxes in basis points (100 bps = 1 %), when measurable.
    pub buy_tax_bps: Option<u32>,
    pub sell_tax_bps: Option<u32>,
    pub transfer_tax_bps: Option<u32>,
}

impl Simulation {
    /// The RPC could not run the simulation: nothing is known about taxes.
    pub fn unavailable() -> Self {
        Self {
            outcome: SimOutcome::Unavailable,
            buy_tax_bps: None,
            sell_tax_bps: None,
            transfer_tax_bps: None,
        }
    }
}

/// Loss between a quote and what was actually received, in basis points (rounded to nearest).
pub fn tax_bps(expected: U256, received: U256) -> Option<u32> {
    if expected.is_zero() {
        return None;
    }
    if received >= expected {
        return Some(0);
    }
    let lost = (expected - received) * U256::from(10_000u32);
    let bps = (lost + expected / U256::from(2u8)) / expected;
    Some(bps.saturating_to::<u32>())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Ownership {
    /// `owner()` returns the zero or dead address.
    Renounced,
    /// `owner()` returns a live address.
    Owned(Address),
    /// The contract has no `owner()` function (roles may still exist).
    NoOwnerFunction,
}

/// Everything the scanner learnt about a freshly listed token.
#[derive(Debug, Clone, Serialize)]
pub struct TokenFacts {
    pub token: Address,
    pub pair: Address,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    /// WBNB side of the pool, in BNB.
    pub liquidity_bnb: f64,
    /// Share of LP tokens burned (sent to the zero or dead address), 0.0–1.0.
    pub lp_burned_ratio: f64,
    pub ownership: Ownership,
    #[serde(skip)]
    pub bytecode: BytecodeReport,
    pub capabilities: Vec<String>,
    pub simulation: Simulation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn from_score(score: u8) -> Self {
        match score {
            75..=100 => Self::Low,
            50..=74 => Self::Medium,
            25..=49 => Self::High,
            _ => Self::Critical,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "🟢 Risque faible",
            Self::Medium => "🟡 Risque moyen",
            Self::High => "🟠 Risque élevé",
            Self::Critical => "🔴 Risque critique",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Assessment {
    pub complete: bool,
    pub score: u8,
    pub level: RiskLevel,
    pub honeypot: bool,
    pub findings: Vec<String>,
}

fn pct(bps: u32) -> String {
    format!("{:.1} %", bps as f64 / 100.0)
}

/// Accumulates findings and the points they cost.
struct Scorer {
    score: i32,
    findings: Vec<String>,
}

impl Scorer {
    fn penalise(&mut self, points: i32, finding: impl Into<String>) {
        self.score -= points;
        self.findings.push(finding.into());
    }

    fn note(&mut self, finding: impl Into<String>) {
        self.findings.push(finding.into());
    }
}

/// Points lost for a buy or sell tax, with the severity word used in the finding.
fn tax_penalty(bps: u32) -> Option<(i32, &'static str)> {
    match bps {
        2_501.. => Some((50, " abusive")),
        1_001..=2_500 => Some((25, " élevée")),
        301..=1_000 => Some((8, "")),
        _ => None,
    }
}

/// Points lost for a privileged function the owner can still call.
fn capability_penalty(cap: Capability) -> i32 {
    match cap {
        Capability::Mint | Capability::Blacklist => 20,
        Capability::FeeControl | Capability::TradingControl => 15,
        Capability::TxLimitControl => 5,
    }
}

/// Scoring rules. Starts at 100 and subtracts a penalty per finding; a honeypot is always 0.
pub fn assess(f: &TokenFacts) -> Assessment {
    let mut s = Scorer {
        score: 100,
        findings: Vec::new(),
    };
    let sim = &f.simulation;

    match sim.outcome {
        SimOutcome::Ok => {}
        SimOutcome::NoRoute => s.penalise(30, "aucune route d'achat (liquidité absente)"),
        SimOutcome::BuyFailed => s.penalise(40, "l'achat échoue en simulation"),
        SimOutcome::Unavailable => s.penalise(10, "simulation impossible avec ce RPC"),
        SimOutcome::TransferFailed => {
            s.note("HONEYPOT : les transferts entre wallets sont bloqués")
        }
        SimOutcome::NothingReceived => s.note("HONEYPOT : l'achat ne livre aucun token"),
        SimOutcome::ApproveFailed | SimOutcome::SellFailed => {
            s.note("HONEYPOT : impossible de revendre après achat")
        }
    }

    for (side, tax) in [
        ("à l'achat", sim.buy_tax_bps),
        ("à la vente", sim.sell_tax_bps),
    ] {
        let Some(t) = tax else { continue };
        if let Some((points, severity)) = tax_penalty(t) {
            s.penalise(points, format!("taxe {side}{severity} : {}", pct(t)));
        }
    }
    if let Some(t) = sim.transfer_tax_bps.filter(|t| *t > 100) {
        s.penalise(
            10,
            format!("taxe sur les transferts entre wallets : {}", pct(t)),
        );
    }

    if f.liquidity_bnb < 0.1 {
        s.penalise(
            35,
            format!("liquidité quasi nulle : {:.3} BNB", f.liquidity_bnb),
        );
    } else if f.liquidity_bnb < 1.0 {
        s.penalise(20, format!("liquidité faible : {:.2} BNB", f.liquidity_bnb));
    }

    if f.lp_burned_ratio < 0.9 {
        s.penalise(
            15,
            format!(
                "liquidité non verrouillée ({:.0} % des LP brûlés)",
                f.lp_burned_ratio * 100.0
            ),
        );
    }

    match f.ownership {
        Ownership::Owned(_) => s.penalise(10, "propriétaire actif (ownership non renoncé)"),
        Ownership::NoOwnerFunction => s.note("pas de fonction owner() (rôles à vérifier)"),
        Ownership::Renounced => {}
    }
    // owner() does not establish whether other privileged roles remain.
    {
        for &cap in &f.bytecode.capabilities {
            s.penalise(capability_penalty(cap), cap.label());
        }
    }
    if f.bytecode.has_delegatecall {
        s.penalise(15, "code modifiable (delegatecall / proxy)");
    }
    if f.bytecode.has_selfdestruct {
        s.penalise(10, "contient selfdestruct");
    }

    let honeypot = sim.outcome.is_honeypot();
    let score = if honeypot {
        0
    } else {
        s.score.clamp(0, 100) as u8
    };
    Assessment {
        complete: sim.outcome != SimOutcome::Unavailable,
        score,
        level: RiskLevel::from_score(score),
        honeypot,
        findings: s.findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_token() -> TokenFacts {
        TokenFacts {
            token: Address::repeat_byte(1),
            pair: Address::repeat_byte(2),
            name: "Safe".into(),
            symbol: "SAFE".into(),
            decimals: 18,
            liquidity_bnb: 25.0,
            lp_burned_ratio: 1.0,
            ownership: Ownership::Renounced,
            bytecode: BytecodeReport {
                is_contract: true,
                size: 5_000,
                ..Default::default()
            },
            capabilities: vec![],
            simulation: Simulation {
                outcome: SimOutcome::Ok,
                buy_tax_bps: Some(0),
                sell_tax_bps: Some(0),
                transfer_tax_bps: Some(0),
            },
        }
    }

    #[test]
    fn tax_is_rounded_basis_points() {
        assert_eq!(tax_bps(U256::from(1000u32), U256::from(950u32)), Some(500));
        assert_eq!(
            tax_bps(U256::from(10_000u32), U256::from(9_501u32)),
            Some(499)
        );
        assert_eq!(tax_bps(U256::from(3u32), U256::from(2u32)), Some(3333));
        assert_eq!(tax_bps(U256::from(100u32), U256::from(120u32)), Some(0));
        assert_eq!(tax_bps(U256::ZERO, U256::ZERO), None);
    }

    #[test]
    fn unavailable_simulation_is_incomplete() {
        let mut token = safe_token();
        token.simulation = Simulation::unavailable();
        assert!(!assess(&token).complete);
    }

    #[test]
    fn clean_token_scores_100() {
        let a = assess(&safe_token());
        assert_eq!(a.score, 100);
        assert_eq!(a.level, RiskLevel::Low);
        assert!(a.findings.is_empty());
    }

    #[test]
    fn honeypot_is_always_zero() {
        let mut t = safe_token();
        t.simulation.outcome = SimOutcome::SellFailed;
        let a = assess(&t);
        assert!(a.honeypot);
        assert_eq!(a.score, 0);
        assert_eq!(a.level, RiskLevel::Critical);
        assert!(a.findings[0].starts_with("HONEYPOT"));
    }

    #[test]
    fn high_taxes_are_penalised() {
        let mut t = safe_token();
        t.simulation.buy_tax_bps = Some(500);
        t.simulation.sell_tax_bps = Some(3_000);
        let a = assess(&t);
        assert_eq!(a.score, 100 - 8 - 50);
        assert_eq!(a.findings.len(), 2);
    }

    #[test]
    fn capabilities_count_even_when_owner_is_renounced() {
        let mut t = safe_token();
        t.bytecode.capabilities = vec![Capability::Mint, Capability::Blacklist];
        assert_eq!(
            assess(&t).score,
            60,
            "renounced ownership does not revoke other roles"
        );
        t.ownership = Ownership::Owned(Address::repeat_byte(9));
        assert_eq!(assess(&t).score, 100 - 10 - 20 - 20);
    }

    #[test]
    fn typical_rug_setup_is_critical() {
        let mut t = safe_token();
        t.liquidity_bnb = 0.5;
        t.lp_burned_ratio = 0.0;
        t.ownership = Ownership::Owned(Address::repeat_byte(9));
        t.bytecode.capabilities = vec![Capability::Mint];
        t.simulation.sell_tax_bps = Some(1_500);
        let a = assess(&t);
        assert_eq!(a.score, 100 - 25 - 20 - 15 - 10 - 20);
        assert_eq!(a.level, RiskLevel::Critical);
        t.bytecode.capabilities.push(Capability::FeeControl);
        assert_eq!(assess(&t).score, 0, "score is clamped at zero");
    }

    #[test]
    fn readme_example_scores_30() {
        let mut t = safe_token();
        t.liquidity_bnb = 0.85;
        t.lp_burned_ratio = 0.0;
        t.ownership = Ownership::Owned(Address::repeat_byte(9));
        t.simulation.buy_tax_bps = Some(200);
        t.simulation.sell_tax_bps = Some(1_200);
        let a = assess(&t);
        assert_eq!(a.score, 30);
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(a.findings.len(), 4);
    }

    #[test]
    fn levels_follow_score_bands() {
        assert_eq!(RiskLevel::from_score(75), RiskLevel::Low);
        assert_eq!(RiskLevel::from_score(74), RiskLevel::Medium);
        assert_eq!(RiskLevel::from_score(49), RiskLevel::High);
        assert_eq!(RiskLevel::from_score(24), RiskLevel::Critical);
    }
}
