#!/usr/bin/env node

import { constants } from "node:os";
import { createRequire } from "node:module";
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const vitePackageJsonPath = require.resolve("vite/package.json");
const vitePackageJson = require(vitePackageJsonPath);
const viteBin = join(dirname(vitePackageJsonPath), vitePackageJson.bin.vite);
const shutdownSignals = new Set(["SIGINT", "SIGTERM"]);

let shutdownRequested = false;
let forceKillTimer = undefined;

const child = spawn(process.execPath, [viteBin, ...process.argv.slice(2)], {
  stdio: "inherit",
  env: process.env,
});

function requestShutdown(signal) {
  shutdownRequested = true;

  if (child.exitCode !== null || child.signalCode !== null) {
    return;
  }

  child.kill(signal);
  forceKillTimer = setTimeout(() => {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGKILL");
    }
  }, 5000);
  forceKillTimer.unref?.();
}

for (const signal of shutdownSignals) {
  process.on(signal, () => requestShutdown(signal));
}

child.on("error", (err) => {
  console.error(`Failed to start Vite dev server: ${err.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (forceKillTimer) {
    clearTimeout(forceKillTimer);
  }

  if (shutdownRequested && signal && shutdownSignals.has(signal)) {
    process.exit(0);
  }

  const expectedShutdownCodes = [...shutdownSignals].map(
    (shutdownSignal) => 128 + constants.signals[shutdownSignal],
  );
  // Tauri intentionally stops beforeDevCommand on dev-app shutdown; Vite may
  // report that as a numeric signal exit instead of `signal`.
  if (shutdownRequested && (code === 0 || expectedShutdownCodes.includes(code))) {
    process.exit(0);
  }

  if (signal) {
    const signalNumber = constants.signals[signal] ?? 1;
    process.exit(128 + signalNumber);
  }

  process.exit(code ?? 1);
});
