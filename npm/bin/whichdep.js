#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const { spawnSync } = require("node:child_process");
const { binaryPath, downloadBinary } = require("./install.js");

async function main() {
  let bin = binaryPath();
  if (!fs.existsSync(bin)) {
    const ok = await downloadBinary({ silent: false });
    if (!ok) {
      // Fall back to a globally installed `whichdep` binary on PATH if present.
      bin = "whichdep";
    }
  }

  const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
  if (result.error) {
    console.error(
      `whichdep: failed to execute native binary (${result.error.message}). Install via \`cargo install whichdep\` or check your network connection.`
    );
    process.exit(2);
  }
  process.exit(result.status === null ? 1 : result.status);
}

main();
