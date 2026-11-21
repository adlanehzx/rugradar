//! Bounded evidence collection. Recent holders are a sample, not a full index.
use alloy::primitives::{Address, U256, keccak256};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub async fn rpc(url: &str, method: &str, params: Value) -> Result<Value> {
    let body: Value = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("RPC transport unavailable"))?
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("RPC response invalid"))?;
    if body.get("error").is_some() {
        bail!("RPC method rejected: {method}");
    }
    body.get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("RPC result missing"))
}
fn address_word(value: &Value) -> Option<Address> {
    let word = value.as_str()?;
    if word.len() != 66 {
        return None;
    }
    let bytes = alloy::hex::decode(word).ok()?;
    if bytes.len() != 32 || bytes[..12].iter().any(|b| *b != 0) {
        return None;
    }
    let a = Address::from_slice(&bytes[12..]);
    (a != Address::ZERO).then_some(a)
}
async fn call(
    url: &str,
    token: Address,
    signature: &str,
    args: &str,
    block: &str,
) -> Result<Value> {
    let selector = crate::bytecode::selector(signature);
    rpc(url,"eth_call",json!([{"to":token.to_string(),"data":format!("0x{}{}",alloy::hex::encode(selector),args)},block])).await
}
fn uint(v: &Value) -> Option<U256> {
    v.as_str()?.parse().ok()
}

pub async fn collect(url: &str, token: Address, pair: Address) -> Value {
    let mut result = json!({"coverage":"recent_sample", "holder_index_complete":false,"locks":{"status":"unknown","reason":"No verified locker adapter configured"},"limitations":["Recent logs only; missing addresses and custom roles are possible","Contract responses are untrusted; no safety guarantee"]});
    let Ok(head) = rpc(url, "eth_blockNumber", json!([])).await else {
        return result;
    };
    let Some(head) = head
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
    else {
        return result;
    };
    let snapshot = format!("0x{head:x}");
    result["snapshot_block"] = json!(head);
    for (name, slot) in [
        (
            "implementation",
            "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
        ),
        (
            "proxy_admin",
            "0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103",
        ),
    ] {
        match rpc(
            url,
            "eth_getStorageAt",
            json!([token.to_string(), slot, snapshot]),
        )
        .await
        {
            Ok(v) => {
                result[name] = json!({"status":"queried","address":address_word(&v)});
            }
            Err(_) => {
                result[name] = json!({"status":"unavailable"});
            }
        }
    }
    if result["implementation"]["address"].is_null() {
        let slot = "0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50";
        if let Ok(value) = rpc(
            url,
            "eth_getStorageAt",
            json!([token.to_string(), slot, snapshot]),
        )
        .await
            && let Some(beacon) = address_word(&value)
        {
            result["beacon"] = json!(beacon);
            if let Ok(value) = call(url, beacon, "implementation()", "", &snapshot).await {
                result["implementation"] =
                    json!({"status":"queried_beacon","address":address_word(&value)});
            }
        }
    }
    if let Some(implementation) = result["implementation"]["address"]
        .as_str()
        .and_then(|s| s.parse::<Address>().ok())
        && let Ok(code) = rpc(
            url,
            "eth_getCode",
            json!([implementation.to_string(), snapshot]),
        )
        .await
        && let Some(code) = code.as_str().and_then(|s| alloy::hex::decode(s).ok())
    {
        let report = crate::bytecode::analyze(&code);
        result["implementation_capabilities"] = json!(
            report
                .capabilities
                .iter()
                .map(|c| c.label())
                .collect::<Vec<_>>()
        );
    }
    let from = head.saturating_sub(300);
    result["from_block"] = json!(from);
    result["to_block"] = json!(head);
    let mut candidates = BTreeSet::new();
    let mut role_candidates = BTreeSet::new();

    let mut logs_ok = true;
    for start in (from..=head).step_by(100) {
        let topic = keccak256("Transfer(address,address,uint256)");
        match rpc(url,"eth_getLogs",json!([{"address":token.to_string(),"fromBlock":format!("0x{start:x}"),"toBlock":format!("0x{:x}",(start+99).min(head)),"topics":[topic.to_string()]}])).await {
            Ok(logs)=>for log in logs.as_array().into_iter().flatten().take(2000) {
                for index in [1,2] {if let Some(a)=address_word(&log["topics"][index]) {candidates.insert(a);}}
            }, Err(_)=>logs_ok=false,
        }
        let topic = keccak256("RoleGranted(bytes32,address,address)");
        if let Ok(logs)=rpc(url,"eth_getLogs",json!([{"address":token.to_string(),"fromBlock":format!("0x{start:x}"),"toBlock":format!("0x{:x}",(start+99).min(head)),"topics":[topic.to_string()]}])).await {
            for log in logs.as_array().into_iter().flatten().take(100) {
                if let (Some(role),Some(account))=(log["topics"][1].as_str(),address_word(&log["topics"][2])) {role_candidates.insert((role.to_owned(),account));}
            }
        }
    }
    // Enumerable access control can expose roles granted before this log window.
    let mut enumerable_roles = Vec::new();
    for role in [
        alloy::primitives::B256::ZERO,
        keccak256("MINTER_ROLE"),
        keccak256("PAUSER_ROLE"),
    ] {
        let role_arg = alloy::hex::encode(role);
        if let Some(count) = call(
            url,
            token,
            "getRoleMemberCount(bytes32)",
            &role_arg,
            &snapshot,
        )
        .await
        .ok()
        .and_then(|v| uint(&v))
        {
            for i in 0..count.saturating_to::<u64>().min(10) {
                let args = format!("{}{:064x}", role_arg, i);
                if let Some(account) = call(
                    url,
                    token,
                    "getRoleMember(bytes32,uint256)",
                    &args,
                    &snapshot,
                )
                .await
                .ok()
                .and_then(|v| address_word(&v))
                {
                    enumerable_roles.push(json!({"role":role,"account":account}));
                }
            }
        }
    }
    result["enumerable_roles"] = json!(enumerable_roles);
    let mut locks = Vec::new();
    if let Ok(addresses) = std::env::var("LP_TIMELOCKS") {
        for locker in addresses
            .split(',')
            .filter_map(|s| s.trim().parse::<Address>().ok())
            .take(10)
        {
            let declared = call(url, locker, "token()", "", &snapshot)
                .await
                .ok()
                .and_then(|v| address_word(&v));
            let release = call(url, locker, "releaseTime()", "", &snapshot)
                .await
                .ok()
                .and_then(|v| uint(&v));
            let args = format!("{:0>64}", alloy::hex::encode(locker));
            let balance = call(url, pair, "balanceOf(address)", &args, &snapshot)
                .await
                .ok()
                .and_then(|v| uint(&v));
            if declared == Some(pair) {
                let mut code_reference = json!({"status":"no_review_reference"});
                if let Ok(path) = std::env::var("LP_LOCKER_REVIEWS")
                    && let Ok(contents) = std::fs::read_to_string(path)
                    && let Ok(entries) = serde_json::from_str::<Vec<Value>>(&contents)
                {
                    for entry in entries.iter().filter(|e| {
                        e["address"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(&locker.to_string()))
                    }) {
                        if let Ok(raw) =
                            rpc(url, "eth_getCode", json!([locker.to_string(), snapshot])).await
                            && let Some(bytes) =
                                raw.as_str().and_then(|s| alloy::hex::decode(s).ok())
                        {
                            let hash = keccak256(&bytes).to_string();
                            code_reference = json!({"status":if entry["runtime_keccak256"]==hash {"reference_match"}else{"reference_mismatch"},"runtime_keccak256":hash,"review_source":entry["source"],"warning":"A matching configured hash is not an independent code audit"});
                        }
                    }
                }
                locks.push(json!({"code_reference":code_reference,"address":locker,"lp_balance":balance.map(|b|b.to_string()),"release_unix":release.map(|t|t.to_string()),"verification":"declared_interface_only","warning":"The interface does not prove immutable locking logic"}));
            }
        }
    }
    if !locks.is_empty() {
        result["locks"] = json!({"status":"declared_timelocks","items":locks});
    }
    let supply = call(url, token, "totalSupply()", "", &snapshot)
        .await
        .ok()
        .and_then(|v| uint(&v));
    let mut holders = Vec::new();
    for account in candidates
        .into_iter()
        .filter(|a| {
            *a != pair
                && *a != alloy::primitives::address!("000000000000000000000000000000000000dead")
        })
        .take(20)
    {
        let args = format!("{:0>64}", alloy::hex::encode(account));
        if let Some(balance) = call(url, token, "balanceOf(address)", &args, &snapshot)
            .await
            .ok()
            .and_then(|v| uint(&v))
        {
            let ratio = supply.filter(|s| !s.is_zero()).map(|s| {
                (balance.saturating_mul(U256::from(10000)) / s).saturating_to::<u64>() as f64
                    / 100.0
            });
            holders.push(
                json!({"address":account,"balance":balance.to_string(),"supply_percent":ratio}),
            );
        }
    }
    holders.sort_by(|a, b| {
        b["supply_percent"]
            .as_f64()
            .partial_cmp(&a["supply_percent"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    result["sampled_holders"] = json!(holders);
    result["holder_logs_available"] = json!(logs_ok);
    let mut roles = Vec::new();
    for (role, account) in role_candidates.into_iter().take(20) {
        let args = format!(
            "{}{:0>64}",
            role.trim_start_matches("0x"),
            alloy::hex::encode(account)
        );
        if call(url, token, "hasRole(bytes32,address)", &args, &snapshot)
            .await
            .ok()
            .and_then(|v| uint(&v))
            == Some(U256::from(1))
        {
            roles.push(json!({"role":role,"account":account}));
        }
    }
    result["active_roles_sample"] = json!(roles);
    result["roles_coverage"] = json!({"status":"partial","absence_proven":false,"note":"Empty results do not establish absence of roles; queries may fail or omit historical/custom roles"});
    result["holder_sampling"] = json!({"limit":20,"selection":"lexicographic addresses seen in recent transfers","top_holders_proven":false});
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_rpc_addresses_are_rejected_without_panics() {
        assert_eq!(
            address_word(&json!(
                "0x0000000000000000000000000000000000000000000000000000000000000000"
            )),
            None
        );
        assert_eq!(address_word(&json!("é".repeat(33))), None);
        assert_eq!(
            address_word(&json!(
                "0x0000000000000000000000001111111111111111111111111111111111111111"
            )),
            Some(Address::repeat_byte(0x11))
        );
    }
    #[tokio::test]
    async fn rpc_transport_failure_is_sanitized() {
        let err = rpc(
            "http://127.0.0.1:1/secret-api-key",
            "eth_chainId",
            json!([]),
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().contains("secret-api-key"));
    }
}
