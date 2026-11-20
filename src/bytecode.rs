//! Static analysis of deployed EVM bytecode: which privileged functions a token exposes,
//! and whether it can delegate execution (proxy) or self-destruct.
use alloy::primitives::keccak256;
use std::collections::HashSet;

/// A family of owner-only capabilities that can hurt holders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    Mint,
    Blacklist,
    FeeControl,
    TradingControl,
    TxLimitControl,
}

impl Capability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mint => "peut créer de nouveaux tokens (mint)",
            Self::Blacklist => "peut bloquer des adresses (blacklist)",
            Self::FeeControl => "peut modifier les taxes",
            Self::TradingControl => "peut suspendre le trading",
            Self::TxLimitControl => "peut limiter la taille des transactions",
        }
    }
}

const SIGNATURES: &[(Capability, &str)] = &[
    (Capability::Mint, "mint(address,uint256)"),
    (Capability::Mint, "mint(uint256)"),
    (Capability::Blacklist, "blacklist(address)"),
    (Capability::Blacklist, "addToBlacklist(address)"),
    (Capability::Blacklist, "setBlacklist(address,bool)"),
    (Capability::Blacklist, "blacklistAddress(address,bool)"),
    (Capability::Blacklist, "setBots(address[])"),
    (Capability::Blacklist, "addBots(address[])"),
    (Capability::Blacklist, "setBot(address,bool)"),
    (Capability::Blacklist, "blockBots(address[])"),
    (Capability::FeeControl, "setFee(uint256)"),
    (Capability::FeeControl, "setFees(uint256,uint256)"),
    (Capability::FeeControl, "setTaxFeePercent(uint256)"),
    (Capability::FeeControl, "setBuyFee(uint256)"),
    (Capability::FeeControl, "setSellFee(uint256)"),
    (Capability::FeeControl, "updateFees(uint256,uint256)"),
    (Capability::FeeControl, "setTax(uint256,uint256)"),
    (Capability::FeeControl, "setTaxes(uint256,uint256)"),
    (Capability::TradingControl, "pause()"),
    (Capability::TradingControl, "setTradingEnabled(bool)"),
    (Capability::TradingControl, "setTrading(bool)"),
    (Capability::TradingControl, "enableTrading(bool)"),
    (Capability::TxLimitControl, "setMaxTxAmount(uint256)"),
    (Capability::TxLimitControl, "setMaxTxPercent(uint256)"),
    (Capability::TxLimitControl, "setMaxWalletSize(uint256)"),
    (Capability::TxLimitControl, "setMaxWallet(uint256)"),
];

/// Four-byte function selector of a canonical signature.
pub fn selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BytecodeReport {
    pub is_contract: bool,
    pub size: usize,
    pub capabilities: Vec<Capability>,
    pub has_delegatecall: bool,
    pub has_selfdestruct: bool,
}

/// Removes the trailing CBOR metadata appended by solc (its length is in the last two bytes),
/// so that its bytes are not mistaken for opcodes.
fn strip_metadata(code: &[u8]) -> &[u8] {
    if code.len() < 2 {
        return code;
    }
    let len = u16::from_be_bytes([code[code.len() - 2], code[code.len() - 1]]) as usize;
    if len + 2 <= code.len() && code.len() - len - 2 > 0 {
        let start = code.len() - len - 2;
        // CBOR maps written by solc start with 0xa1..0xa5.
        if (0xa1..=0xa5).contains(&code[start]) {
            return &code[..start];
        }
    }
    code
}

pub fn analyze(code: &[u8]) -> BytecodeReport {
    if code.is_empty() {
        return BytecodeReport::default();
    }
    let body = strip_metadata(code);
    let mut pushed: HashSet<[u8; 4]> = HashSet::new();
    let mut report = BytecodeReport {
        is_contract: true,
        size: code.len(),
        ..Default::default()
    };

    let mut i = 0;
    while i < body.len() {
        let op = body[i];
        match op {
            0x60..=0x7f => {
                let n = (op - 0x5f) as usize;
                if n == 4 && i + 4 < body.len() {
                    pushed.insert([body[i + 1], body[i + 2], body[i + 3], body[i + 4]]);
                }
                i += n;
            }
            0xf4 => report.has_delegatecall = true,
            0xff => report.has_selfdestruct = true,
            _ => {}
        }
        i += 1;
    }

    let mut caps: Vec<Capability> = SIGNATURES
        .iter()
        .filter(|(_, sig)| pushed.contains(&selector(sig)))
        .map(|(cap, _)| *cap)
        .collect();
    caps.sort();
    caps.dedup();
    report.capabilities = caps;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a fake dispatcher: `PUSH4 <selector> EQ` for every signature.
    fn dispatcher(signatures: &[&str], extra: &[u8]) -> Vec<u8> {
        let mut code = vec![0x60, 0x80, 0x60, 0x40, 0x52];
        for sig in signatures {
            code.push(0x63);
            code.extend_from_slice(&selector(sig));
            code.push(0x14);
        }
        code.extend_from_slice(extra);
        code
    }

    #[test]
    fn known_selector_values() {
        assert_eq!(
            selector("transfer(address,uint256)"),
            [0xa9, 0x05, 0x9c, 0xbb]
        );
        assert_eq!(selector("mint(address,uint256)"), [0x40, 0xc1, 0x0f, 0x19]);
    }

    #[test]
    fn plain_token_has_no_capability() {
        let r = analyze(&dispatcher(
            &["transfer(address,uint256)", "approve(address,uint256)"],
            &[],
        ));
        assert!(r.is_contract);
        assert!(r.capabilities.is_empty());
        assert!(!r.has_delegatecall && !r.has_selfdestruct);
    }

    #[test]
    fn detects_privileged_functions() {
        let r = analyze(&dispatcher(
            &[
                "mint(address,uint256)",
                "setBots(address[])",
                "setFees(uint256,uint256)",
            ],
            &[],
        ));
        assert_eq!(
            r.capabilities,
            vec![
                Capability::Mint,
                Capability::Blacklist,
                Capability::FeeControl
            ]
        );
    }

    #[test]
    fn push_data_is_not_read_as_opcodes() {
        // PUSH2 0xf4ff: the bytes are data, not DELEGATECALL / SELFDESTRUCT.
        let r = analyze(&dispatcher(&[], &[0x61, 0xf4, 0xff]));
        assert!(!r.has_delegatecall && !r.has_selfdestruct);
        let r = analyze(&dispatcher(&[], &[0xf4]));
        assert!(r.has_delegatecall);
    }

    #[test]
    fn metadata_is_ignored() {
        // Body, then a 3-byte "CBOR" blob containing 0xff, then its length.
        let mut code = dispatcher(&[], &[0x00]);
        code.extend_from_slice(&[0xa1, 0xff, 0xff, 0x00, 0x03]);
        assert!(!analyze(&code).has_selfdestruct);
    }

    #[test]
    fn empty_code_is_not_a_contract() {
        assert!(!analyze(&[]).is_contract);
    }
}
