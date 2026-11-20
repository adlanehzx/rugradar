// End-to-end tests of Simulator.sol on a local in-memory EVM (ganache) with the official
// Uniswap V2 factory/router artifacts, which PancakeSwap V2 is a fork of.
// Run: cd contracts && npm install && npm test
import { test, before } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import solc from "solc";
import ganache from "ganache";
import { ethers } from "ethers";

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, "..");

const Factory = require("@uniswap/v2-core/build/UniswapV2Factory.json");
const Router = require("@uniswap/v2-periphery/build/UniswapV2Router02.json");
const WETH = require("@uniswap/v2-periphery/build/WETH9.json");

function compile() {
  const read = (p) => fs.readFileSync(p, "utf8");
  const input = {
    language: "Solidity",
    sources: {
      "Simulator.sol": { content: read(path.join(root, "Simulator.sol")) },
      "MockTokens.sol": { content: read(path.join(here, "mocks", "MockTokens.sol")) },
    },
    settings: {
      optimizer: { enabled: true, runs: 200 },
      evmVersion: "paris",
      outputSelection: { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
    },
  };
  const findImports = (p) => {
    try {
      return { contents: read(require.resolve(p)) };
    } catch {
      return { error: `not found: ${p}` };
    }
  };
  const out = JSON.parse(solc.compile(JSON.stringify(input), { import: findImports }));
  const errors = (out.errors ?? []).filter((e) => e.severity === "error");
  if (errors.length) throw new Error(errors.map((e) => e.formattedMessage).join("\n"));
  return { ...out.contracts["Simulator.sol"], ...out.contracts["MockTokens.sol"] };
}

const SIM_ADDRESS = "0x00000000000000000000000000000000005151aa";
const CALLER = "0x0000000000000000000000000000000000c0ffee";
const BUY_VALUE = ethers.parseEther("0.1");

let provider, deployer, artifacts, weth, factory, router, simIface;

async function deploy(abi, bytecode, ...args) {
  const f = new ethers.ContractFactory(abi, bytecode, deployer);
  const c = await f.deploy(...args);
  await c.waitForDeployment();
  return c;
}

/** Adds TOKEN/WETH liquidity and registers the pair on taxed / honeypot tokens. */
async function list(token, { hasPair = false } = {}) {
  const tokenAddr = await token.getAddress();
  await (await token.approve(await router.getAddress(), ethers.MaxUint256)).wait();
  await (
    await router.addLiquidityETH(tokenAddr, ethers.parseEther("100000"), 0, 0, deployer.address, 2n ** 40n, {
      value: ethers.parseEther("10"),
    })
  ).wait();
  if (hasPair) {
    const pair = await factory.getPair(tokenAddr, await weth.getAddress());
    await (await token.setPair(pair)).wait();
  }
  return tokenAddr;
}

/** Same call the Rust scanner makes: Simulator bytecode injected with a state override. */
async function simulate(tokenAddr) {
  const data = simIface.encodeFunctionData("simulate", [
    await router.getAddress(),
    tokenAddr,
    await weth.getAddress(),
  ]);
  const raw = await provider.send("eth_call", [
    { from: CALLER, to: SIM_ADDRESS, data, value: ethers.toQuantity(BUY_VALUE), gas: ethers.toQuantity(8_000_000) },
    "latest",
    {
      [SIM_ADDRESS]: { code: "0x" + artifacts.Simulator.evm.deployedBytecode.object },
      [CALLER]: { balance: ethers.toQuantity(ethers.parseEther("10")) },
    },
  ]);
  return simIface.decodeFunctionResult("simulate", raw)[0];
}

const bps = (expected, received) => (expected === 0n ? 0n : ((expected - received) * 10_000n) / expected);
const near = (value, target, tolerance = 10n) =>
  assert.ok(value >= target - tolerance && value <= target + tolerance, `${value} bps, expected ~${target}`);

before(async () => {
  artifacts = compile();
  const chain = ganache.provider({ logging: { quiet: true }, wallet: { totalAccounts: 1, defaultBalance: 1_000_000 } });
  provider = new ethers.BrowserProvider(chain);
  deployer = await provider.getSigner(0);
  simIface = new ethers.Interface(artifacts.Simulator.abi);

  weth = await deploy(WETH.abi, WETH.bytecode);
  factory = await deploy(Factory.abi, Factory.bytecode, deployer.address);
  // The router hardcodes the pair init-code hash of the official factory build used here.
  router = await deploy(Router.abi, Router.bytecode, await factory.getAddress(), await weth.getAddress());
});

test("committed runtime bytecode matches the source", () => {
  const committed = fs.readFileSync(path.join(root, "..", "src", "simulator", "Simulator.bin"), "utf8").trim();
  assert.equal(committed, artifacts.Simulator.evm.deployedBytecode.object, "run `npm run build` in contracts/");
});

test("plain token: buy and sell succeed without tax", async () => {
  const token = await deploy(artifacts.NormalToken.abi, artifacts.NormalToken.evm.bytecode.object);
  const r = await simulate(await list(token));
  assert.equal(r.status, 0n);
  assert.equal(r.buyReceived, r.buyExpected);
  assert.equal(r.sellReceived, r.sellExpected);
  assert.equal(r.transferReceived, r.transferSent);
});

test("taxed token: buy, sell and transfer taxes are measured", async () => {
  const token = await deploy(artifacts.TaxToken.abi, artifacts.TaxToken.evm.bytecode.object, 500, 200);
  const r = await simulate(await list(token, { hasPair: true }));
  assert.equal(r.status, 0n);
  near(bps(r.buyExpected, r.buyReceived), 500n);
  near(bps(r.transferSent, r.transferReceived), 200n);
  // The router quote ignores the tax, so a sell taxed at 5% receives ~5% less than quoted.
  near(bps(r.sellExpected, r.sellReceived), 500n);
});

test("honeypot token: sell is detected as blocked", async () => {
  const token = await deploy(artifacts.HoneypotToken.abi, artifacts.HoneypotToken.evm.bytecode.object);
  const r = await simulate(await list(token, { hasPair: true }));
  assert.equal(r.status, 4n);
  assert.ok(r.buyReceived > 0n);
});

test("token without liquidity: reported as no route", async () => {
  const token = await deploy(artifacts.NormalToken.abi, artifacts.NormalToken.evm.bytecode.object);
  const r = await simulate(await token.getAddress());
  assert.equal(r.status, 1n);
});
