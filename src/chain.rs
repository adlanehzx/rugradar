//! Everything that talks to the BNB Smart Chain RPC.
use crate::abi::{IERC20Meta, IOwnable, IPancakeFactory, IPancakePair, ISimulator};
use crate::analysis::{Ownership, SimOutcome, Simulation, tax_bps};
use crate::bytecode::{self, BytecodeReport};
use crate::config::Config;
use alloy::{
    network::TransactionBuilder,
    primitives::{Address, U256, address},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::{Filter, TransactionRequest, state::StateOverridesBuilder},
    sol_types::{SolCall, SolEvent},
};
use anyhow::{Context, Result};

/// Arbitrary, empty addresses used for the simulated call.
const SIMULATOR: Address = address!("00000000000000000000000000000000005151aa");
const CALLER: Address = address!("0000000000000000000000000000000000c0ffee");
const DEAD: Address = address!("000000000000000000000000000000000000dEaD");

/// Runtime bytecode of `contracts/Simulator.sol` (rebuilt with `npm run build` in contracts/).
const SIMULATOR_RUNTIME: &str = include_str!("simulator/Simulator.bin");

/// A pair listed on PancakeSwap V2 against WBNB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewPair {
    pub token: Address,
    pub pair: Address,
    pub block: u64,
}

#[derive(Clone)]
pub struct Chain {
    providers: Vec<DynProvider>,
    active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    expected_chain: u64,
    simulator_code: Vec<u8>,
    factory: Address,
    router: Address,
    wrapped_native: Address,
}

impl Chain {
    pub fn connect(config: &Config) -> Result<Self> {
        let url = config
            .rpc_url
            .parse()
            .context("RPC_URL is not a valid URL")?;
        let provider = ProviderBuilder::new().connect_http(url).erased();
        let mut providers = vec![provider];
        for url in std::env::var("RPC_FALLBACK_URLS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
        {
            providers.push(
                ProviderBuilder::new()
                    .connect_http(url.trim().parse().context("invalid fallback RPC URL")?)
                    .erased(),
            );
        }
        let simulator_code =
            alloy::hex::decode(SIMULATOR_RUNTIME.trim()).context("invalid Simulator.bin")?;
        Ok(Self {
            providers,
            active: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            expected_chain: config.chain_id,
            simulator_code,
            factory: config.factory,
            router: config.router,
            wrapped_native: config.wrapped_native,
        })
    }

    fn provider(&self) -> DynProvider {
        self.providers[self.active.load(std::sync::atomic::Ordering::Relaxed)].clone()
    }
    fn rotate(&self) {
        let current = self.active.load(std::sync::atomic::Ordering::Relaxed);
        self.active.store(
            (current + 1) % self.providers.len(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub async fn chain_id(&self) -> Result<u64> {
        Ok(self.provider().get_chain_id().await?)
    }

    pub async fn latest_block(&self) -> Result<u64> {
        for _ in 0..self.providers.len() {
            let provider = self.provider();
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                anyhow::ensure!(
                    provider.get_chain_id().await? == self.expected_chain,
                    "RPC chain mismatch"
                );
                Ok::<u64, anyhow::Error>(provider.get_block_number().await?)
            })
            .await;
            if let Ok(Ok(head)) = result {
                return Ok(head);
            }
            self.rotate();
        }
        anyhow::bail!("All configured RPC endpoints unavailable")
    }

    /// `PairCreated` events in `[from, to]`, keeping only TOKEN/WBNB pairs.
    pub async fn new_pairs(&self, from: u64, to: u64) -> Result<Vec<NewPair>> {
        let filter = Filter::new()
            .address(self.factory)
            .event_signature(IPancakeFactory::PairCreated::SIGNATURE_HASH)
            .from_block(from)
            .to_block(to);
        let logs = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.provider().get_logs(&filter),
        )
        .await
        {
            Ok(Ok(logs)) => logs,
            _ => {
                self.rotate();
                anyhow::bail!("RPC log query failed; endpoint rotated");
            }
        };
        let mut pairs = Vec::new();
        for log in logs {
            let block = log.block_number.unwrap_or(to);
            let Ok(event) = IPancakeFactory::PairCreated::decode_log(&log.inner) else {
                continue;
            };
            let (t0, t1) = (event.token0, event.token1);
            let native = self.wrapped_native;
            let token = match (t0 == native, t1 == native) {
                (true, false) => t1,
                (false, true) => t0,
                _ => continue, // not paired with the wrapped native token
            };
            pairs.push(NewPair {
                token,
                pair: event.pair,
                block,
            });
        }
        Ok(pairs)
    }

    /// Existing TOKEN/WBNB pair, if any.
    pub async fn pair_for(&self, token: Address) -> Result<Option<Address>> {
        let pair = IPancakeFactory::new(self.factory, &self.provider())
            .getPair(token, self.wrapped_native)
            .call()
            .await?;
        Ok((pair != Address::ZERO).then_some(pair))
    }

    pub async fn token_metadata(&self, token: Address) -> (String, String, u8) {
        let provider = self.provider();
        let erc20 = IERC20Meta::new(token, &provider);
        let name = erc20.name().call().await.unwrap_or_else(|_| "?".into());
        let symbol = erc20.symbol().call().await.unwrap_or_else(|_| "?".into());
        let decimals = erc20.decimals().call().await.unwrap_or(18);
        (name, symbol, decimals)
    }

    pub async fn ownership(&self, token: Address) -> Ownership {
        match IOwnable::new(token, &self.provider()).owner().call().await {
            Ok(owner) if owner == Address::ZERO || owner == DEAD => Ownership::Renounced,
            Ok(owner) => Ownership::Owned(owner),
            Err(_) => Ownership::NoOwnerFunction,
        }
    }

    pub async fn bytecode(&self, token: Address) -> Result<BytecodeReport> {
        let code = self.provider().get_code_at(token).await?;
        Ok(bytecode::analyze(&code))
    }

    /// Wrapped-native reserve of the pair (in BNB) and the share of LP tokens that were burned.
    pub async fn liquidity(&self, pair: Address, token: Address) -> Result<(f64, f64)> {
        let provider = self.provider();
        let lp = IPancakePair::new(pair, &provider);
        let reserves = lp.getReserves().call().await?;
        let native_reserve = if token < self.wrapped_native {
            reserves.reserve1
        } else {
            reserves.reserve0
        };
        let liquidity_bnb = native_reserve.to::<u128>() as f64 / 1e18;

        let supply = lp.totalSupply().call().await?;
        let burned = lp.balanceOf(DEAD).call().await? + lp.balanceOf(Address::ZERO).call().await?;
        let ratio = if supply.is_zero() {
            0.0
        } else {
            u256_ratio(burned, supply)
        };
        Ok((liquidity_bnb, ratio))
    }

    /// Buys, transfers and sells the token inside a single `eth_call`, with the Simulator
    /// bytecode injected through a state override. Costs nothing and needs no wallet.
    pub async fn simulate(&self, token: Address, buy_value: U256) -> Simulation {
        let Ok(block) = self.latest_block().await else {
            return Simulation::unavailable();
        };
        let amounts = [
            (buy_value / U256::from(10)).max(U256::from(1)),
            buy_value,
            buy_value.saturating_mul(U256::from(10)),
        ];
        let addresses = [
            (CALLER, SIMULATOR),
            (
                address!("0000000000000000000000000000000000c0ffef"),
                address!("00000000000000000000000000000000005151ab"),
            ),
        ];
        let mut trials = Vec::new();
        for (caller, simulator) in addresses {
            for value in amounts {
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(8),
                    self.simulate_one(token, value, caller, simulator, block),
                )
                .await
                .unwrap_or_else(|_| Simulation::unavailable());
                trials.push(crate::analysis::Trial {
                    value_wei: value.to_string(),
                    caller,
                    simulator,
                    outcome: result.outcome,
                    buy_tax_bps: result.buy_tax_bps,
                    sell_tax_bps: result.sell_tax_bps,
                    transfer_tax_bps: result.transfer_tax_bps,
                });
            }
        }
        summarize_trials(trials, block)
    }
    async fn simulate_one(
        &self,
        token: Address,
        buy_value: U256,
        caller: Address,
        simulator: Address,
        block: u64,
    ) -> Simulation {
        let data = ISimulator::simulateCall {
            router: self.router,
            token,
            wrappedNative: self.wrapped_native,
        }
        .abi_encode();
        let tx = TransactionRequest::default()
            .with_from(caller)
            .with_to(simulator)
            .with_value(buy_value)
            .with_gas_limit(8_000_000)
            .with_input(data);
        let overrides = StateOverridesBuilder::default()
            .with_code(simulator, self.simulator_code.clone())
            .with_balance(caller, buy_value * U256::from(10u8))
            .build();

        let raw = match self
            .provider()
            .call(tx)
            .block(block.into())
            .overrides(overrides)
            .await
        {
            Ok(raw) => raw,
            Err(err) => {
                tracing::debug!(%token, %err, "simulation call failed");
                return Simulation::unavailable();
            }
        };
        match ISimulator::simulateCall::abi_decode_returns(&raw) {
            Ok(r) => Simulation {
                trials: vec![],
                block: Some(block),
                outcome: SimOutcome::from_status(r.status),
                buy_tax_bps: tax_bps(r.buyExpected, r.buyReceived),
                sell_tax_bps: tax_bps(r.sellExpected, r.sellReceived),
                transfer_tax_bps: tax_bps(r.transferSent, r.transferReceived),
            },
            Err(_) => Simulation::unavailable(),
        }
    }
}

fn summarize_trials(trials: Vec<crate::analysis::Trial>, block: u64) -> Simulation {
    // A successful scenario must never conceal a failed or unavailable scenario.
    let outcome = trials
        .iter()
        .find(|t| t.outcome.is_honeypot())
        .map(|t| t.outcome)
        .or_else(|| {
            trials
                .iter()
                .find(|t| t.outcome != SimOutcome::Ok)
                .map(|t| t.outcome)
        })
        .unwrap_or(SimOutcome::Unavailable);
    let outcome = if trials.iter().all(|t| t.outcome == SimOutcome::Ok) && !trials.is_empty() {
        SimOutcome::Ok
    } else {
        outcome
    };
    Simulation {
        outcome,
        buy_tax_bps: trials.iter().filter_map(|t| t.buy_tax_bps).max(),
        sell_tax_bps: trials.iter().filter_map(|t| t.sell_tax_bps).max(),
        transfer_tax_bps: trials.iter().filter_map(|t| t.transfer_tax_bps).max(),
        trials,
        block: Some(block),
    }
}

/// `a / b` as a float in [0, 1] without overflowing f64 on 256-bit values.
fn u256_ratio(a: U256, b: U256) -> f64 {
    if b.is_zero() {
        return 0.0;
    }
    let scaled = alloy::primitives::U512::from(a) * alloy::primitives::U512::from(1_000_000u32)
        / alloy::primitives::U512::from(b);
    (scaled.saturating_to::<u64>() as f64 / 1_000_000.0).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn falls_back_after_primary_transport_failure() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buf = [0u8; 4096];
                    let request = loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buf[..n]);
                        if let Some(start) = bytes.windows(4).position(|s| s == b"\r\n\r\n")
                            && let Ok(body) =
                                serde_json::from_slice::<serde_json::Value>(&bytes[start + 4..])
                        {
                            break body;
                        }
                    };
                    let result = if request["method"] == "eth_chainId" {
                        "0x38"
                    } else {
                        "0x64"
                    };
                    let body =
                        serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                            .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        let chain = Chain {
            providers: vec![
                ProviderBuilder::new()
                    .connect_http("http://127.0.0.1:1".parse().unwrap())
                    .erased(),
                ProviderBuilder::new()
                    .connect_http(format!("http://{addr}").parse().unwrap())
                    .erased(),
            ],
            active: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            expected_chain: 56,
            simulator_code: Vec::new(),
            factory: Address::ZERO,
            router: Address::ZERO,
            wrapped_native: Address::ZERO,
        };
        assert_eq!(chain.latest_block().await.unwrap(), 100);
        assert_eq!(chain.active.load(std::sync::atomic::Ordering::Relaxed), 1);
        mock.abort();
    }

    #[test]
    fn embedded_simulator_bytecode_is_valid_hex() {
        let code = alloy::hex::decode(SIMULATOR_RUNTIME.trim()).unwrap();
        assert!(code.len() > 1_000);
        let report = bytecode::analyze(&code);
        assert!(report.is_contract);
        assert!(!report.has_selfdestruct);
    }

    #[test]
    fn ratio_handles_large_values() {
        let supply = U256::from(10u8).pow(U256::from(30u8));
        assert_eq!(u256_ratio(supply, supply), 1.0);
        assert_eq!(u256_ratio(supply / U256::from(4u8), supply), 0.25);
        assert_eq!(u256_ratio(U256::ZERO, supply), 0.0);
        assert_eq!(u256_ratio(U256::from(1u8), U256::ZERO), 0.0);
    }
}
