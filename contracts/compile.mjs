// Compiles Simulator.sol and writes its runtime bytecode to ../src/simulator/Simulator.bin
// Usage: cd contracts && npm install && npm run build
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import solc from "solc";

const here = path.dirname(fileURLToPath(import.meta.url));
const source = fs.readFileSync(path.join(here, "Simulator.sol"), "utf8");

const input = {
  language: "Solidity",
  sources: { "Simulator.sol": { content: source } },
  settings: {
    optimizer: { enabled: true, runs: 200 },
    evmVersion: "paris",
    outputSelection: { "*": { Simulator: ["evm.deployedBytecode.object"] } },
  },
};

const output = JSON.parse(solc.compile(JSON.stringify(input)));
const errors = (output.errors ?? []).filter((e) => e.severity === "error");
if (errors.length) {
  for (const e of errors) console.error(e.formattedMessage);
  process.exit(1);
}

const runtime = output.contracts["Simulator.sol"].Simulator.evm.deployedBytecode.object;
const outFile = path.join(here, "..", "src", "simulator", "Simulator.bin");
fs.mkdirSync(path.dirname(outFile), { recursive: true });
fs.writeFileSync(outFile, runtime + "\n");
console.log(`solc ${solc.version()}: wrote ${runtime.length / 2} bytes to src/simulator/Simulator.bin`);
