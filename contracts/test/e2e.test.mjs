// End-to-end test of the Rust scanner against a local chain.
// A ganache node (chain id 56) gets the official Uniswap V2 factory/router and three tokens
// (plain, taxed, honeypot); the compiled `rugradar` binary then analyses them over JSON-RPC.
// Run: cargo build && cd contracts && npm run test:e2e
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import solc from "solc";
import ganache from "ganache";
import { ethers } from "ethers";

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.join(here, "..", "..");
const binary = path.join(repo, "target", "debug", "rugradar");
const PORT = 18545;

const Factory = require("@uniswap/v2-core/build/UniswapV2Factory.json");
const Router = require("@uniswap/v2-periphery/build/UniswapV2Router02.json");
const WETH = require("@uniswap/v2-periphery/build/WETH9.json");

function compileMocks() {
  const read = (p) => fs.readFileSync(p, "utf8");
  const input = {
    language: "Solidity",
    sources: { "MockTokens.sol": { content: read(path.join(here, "mocks", "MockTokens.sol")) } },
    settings: { optimizer: { enabled: true, runs: 200 }, evmVersion: "paris", outputSelection: { "*": { "*": ["abi", "evm.bytecode.object"] } } },
  };
  const out = JSON.parse(solc.compile(JSON.stringify(input), { import: (p) => ({ contents: read(require.resolve(p)) }) }));
  return out.contracts["MockTokens.sol"];
}

let server, deployer, env, tokens, tmp;

async function deploy(abi, bytecode, ...args) {
  const c = await new ethers.ContractFactory(abi, bytecode, deployer).deploy(...args);
  await c.waitForDeployment();
  return c;
}

// Async on purpose: the ganache node runs in this process, so a blocking call would deadlock it.
async function rugradar(...args) {
  const { stdout } = await promisify(execFile)(binary, args, { env: { ...process.env, ...env }, timeout: 60_000 });
  return stdout;
}

before(async () => {
  assert.ok(fs.existsSync(binary), "build the scanner first: cargo build");
  server = ganache.server({ chain: { chainId: 56 }, logging: { quiet: true }, wallet: { totalAccounts: 1, defaultBalance: 1_000_000 } });
  await server.listen(PORT);
  const provider = new ethers.JsonRpcProvider(`http://127.0.0.1:${PORT}`, undefined, { cacheTimeout: -1 });
  deployer = await provider.getSigner(0);

  const weth = await deploy(WETH.abi, WETH.bytecode);
  const factory = await deploy(Factory.abi, Factory.bytecode, deployer.address);
  const router = await deploy(Router.abi, Router.bytecode, await factory.getAddress(), await weth.getAddress());
  const mocks = compileMocks();

  async function listToken(name, args, { setPair = false, renounce = false } = {}) {
    const token = await deploy(mocks[name].abi, mocks[name].evm.bytecode.object, ...args);
    const addr = await token.getAddress();
    await (await token.approve(await router.getAddress(), ethers.MaxUint256)).wait();
    await (
      await router.addLiquidityETH(addr, ethers.parseEther("100000"), 0, 0, deployer.address, 2n ** 40n, { value: ethers.parseEther("5") })
    ).wait();
    if (setPair) await (await token.setPair(await factory.getPair(addr, await weth.getAddress()))).wait();
    return addr;
  }

  tokens = {
    normal: await listToken("NormalToken", []),
    taxed: await listToken("TaxToken", [1_500, 0], { setPair: true }),
    honeypot: await listToken("HoneypotToken", [], { setPair: true }),
  };

  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "rugradar-e2e-"));
  env = {
    RPC_URL: `http://127.0.0.1:${PORT}`,
    CHAIN_ID: "56",
    CONFIRMATIONS: "0", // The local chain does not mine empty confirmation blocks.
    FACTORY_ADDRESS: await factory.getAddress(),
    ROUTER_ADDRESS: await router.getAddress(),
    WRAPPED_NATIVE_ADDRESS: await weth.getAddress(),
    DATABASE_URL: `sqlite://${path.join(tmp, "e2e.db")}`,
    ALERT_MODE: "off",
    START_LOOKBACK_BLOCKS: "1000",
    POLL_INTERVAL_SECS: "1",
    RUST_LOG: "warn",
  };
});

after(async () => {
  await server?.close();
  if (tmp) fs.rmSync(tmp, { recursive: true, force: true });
});

const check = async (token) => JSON.parse(await rugradar("check", token));

test("plain token: simulation passes with no tax", async () => {
  const { facts, assessment } = await check(tokens.normal);
  assert.equal(facts.symbol, "NORM");
  assert.equal(facts.simulation.outcome, "Ok");
  assert.equal(facts.simulation.buy_tax_bps, 0);
  assert.equal(facts.simulation.sell_tax_bps, 0);
  assert.equal(facts.liquidity_bnb, 5);
  assert.equal(assessment.honeypot, false);
  // The deployer still holds the LP tokens, and a plain OpenZeppelin ERC-20 has no owner().
  assert.deepEqual(assessment.findings, [
    "liquidité non verrouillée (0 % des LP brûlés)",
    "pas de fonction owner() (rôles à vérifier)",
  ]);
  assert.equal(assessment.score, 85);
});

test("taxed token: 15 % taxes are measured and penalised", async () => {
  const { facts, assessment } = await check(tokens.taxed);
  assert.equal(facts.simulation.outcome, "Ok");
  assert.ok(Math.abs(facts.simulation.buy_tax_bps - 1500) <= 10, `buy tax ${facts.simulation.buy_tax_bps}`);
  assert.ok(Math.abs(facts.simulation.sell_tax_bps - 1500) <= 10, `sell tax ${facts.simulation.sell_tax_bps}`);
  assert.ok(facts.ownership.Owned, "TaxToken exposes an active owner");
  assert.ok(assessment.score < 50, `score ${assessment.score}`);
});

test("honeypot token: flagged with a score of 0", async () => {
  const { facts, assessment } = await check(tokens.honeypot);
  assert.equal(facts.simulation.outcome, "SellFailed");
  assert.equal(assessment.honeypot, true);
  assert.equal(assessment.score, 0);
});

test("watch mode: discovers the three pairs from PairCreated events", async () => {
  const child = spawn(binary, [], { env: { ...process.env, ...env } });
  await new Promise((resolve) => setTimeout(resolve, 6_000));
  child.kill();
  assert.match(await rugradar("stats"), /^3 tokens analysed, 1 honeypots detected/);
});
