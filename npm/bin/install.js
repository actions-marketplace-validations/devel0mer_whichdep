#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const pkg = require("../package.json");

const TARGETS = {
  "darwin:arm64": "aarch64-apple-darwin",
  "darwin:x64": "x86_64-apple-darwin",
  "linux:x64": "x86_64-unknown-linux-gnu",
  "linux:arm64": "aarch64-unknown-linux-gnu",
  "win32:x64": "x86_64-pc-windows-msvc",
};

function targetTriple() {
  return TARGETS[`${process.platform}:${process.arch}`] || null;
}

function binaryName() {
  return process.platform === "win32" ? "whichdep.exe" : "whichdep";
}

function binaryPath() {
  const name = process.platform === "win32" ? "whichdep-bin.exe" : "whichdep-bin";
  return path.join(__dirname, name);
}

async function downloadBinary({ silent = false } = {}) {
  const triple = targetTriple();
  if (!triple) {
    if (!silent) {
      console.warn(
        `whichdep: no prebuilt binary for ${process.platform}/${process.arch}; install via \`cargo install whichdep\` or \`uvx whichdep\`.`
      );
    }
    return false;
  }

  const dest = binaryPath();
  if (fs.existsSync(dest)) {
    return true;
  }

  const version = pkg.version;
  const asset = `whichdep-v${version}-${triple}.tar.gz`;
  const url = `https://github.com/devel0mer/whichdep/releases/download/v${version}/${asset}`;

  try {
    const res = await fetch(url, { redirect: "follow" });
    if (!res.ok) {
      if (!silent) {
        console.warn(
          `whichdep: could not download ${url} (HTTP ${res.status}); install via \`cargo install whichdep\` if offline.`
        );
      }
      return false;
    }
    const buf = Buffer.from(await res.arrayBuffer());
    const tmpDir = fs.mkdtempSync(path.join(__dirname, ".tmp-"));
    const archive = path.join(tmpDir, asset);
    try {
      fs.writeFileSync(archive, buf);
      execFileSync("tar", ["-xzf", archive, "-C", tmpDir], { stdio: "ignore" });
      const extracted = path.join(tmpDir, binaryName());
      fs.renameSync(extracted, dest);
      if (process.platform !== "win32") {
        fs.chmodSync(dest, 0o755);
      }
      return true;
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  } catch (err) {
    if (!silent) {
      console.warn(`whichdep: postinstall download skipped (${err.message}).`);
    }
    return false;
  }
}

module.exports = { binaryPath, downloadBinary };

if (require.main === module) {
  downloadBinary().then(() => process.exit(0));
}
