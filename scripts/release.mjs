#!/usr/bin/env node

/**
 * SymbolSweep Release Script
 *
 * Usage: npm run release
 *
 * Prerequisites:
 *   - TAURI_SIGNING_PRIVATE_KEY env var set
 *   - gh CLI installed and authenticated
 *   - Rust toolchains: aarch64-apple-darwin, x86_64-apple-darwin
 */

import { execSync } from "child_process";
import { readFileSync, writeFileSync, existsSync, readdirSync } from "fs";
import { join } from "path";
import { fileURLToPath } from "url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const ROOT = join(__dirname, "..");
const TAURI_DIR = join(ROOT, "src-tauri");
const CONF_PATH = join(TAURI_DIR, "tauri.conf.json");

function run(cmd) {
  console.log(`> ${cmd}`);
  execSync(cmd, { stdio: "inherit", cwd: ROOT });
}

function runCapture(cmd) {
  return execSync(cmd, { encoding: "utf-8", cwd: ROOT }).trim();
}

// ---- Read version ----

const conf = JSON.parse(readFileSync(CONF_PATH, "utf-8"));
const VERSION = conf.version;
const TAG = `v${VERSION}`;
const PRODUCT = conf.productName;

console.log(`\nReleasing ${PRODUCT} ${TAG}\n`);

// ---- Preflight checks ----

if (!process.env.TAURI_SIGNING_PRIVATE_KEY) {
  console.error("ERROR: TAURI_SIGNING_PRIVATE_KEY env var not set.");
  console.error(
    "Run: export TAURI_SIGNING_PRIVATE_KEY=$(cat ~/.tauri/symbolsweep.key)"
  );
  process.exit(1);
}

try {
  runCapture("gh --version");
} catch {
  console.error("ERROR: gh CLI not installed. Run: brew install gh");
  process.exit(1);
}

try {
  const targets = runCapture("rustup target list --installed");
  if (!targets.includes("aarch64-apple-darwin")) {
    throw new Error("missing aarch64");
  }
  if (!targets.includes("x86_64-apple-darwin")) {
    throw new Error("missing x86_64");
  }
} catch {
  console.error("ERROR: Missing Rust targets. Run:");
  console.error("  rustup target add aarch64-apple-darwin");
  console.error("  rustup target add x86_64-apple-darwin");
  process.exit(1);
}

// Check for uncommitted changes
try {
  const status = runCapture("git status --porcelain");
  if (status) {
    console.error("ERROR: Uncommitted changes detected. Commit or stash first.");
    console.error(status);
    process.exit(1);
  }
} catch {
  // Not a git repo or git not available, continue
}

// ---- Build universal binary ----

console.log("\n=== Building universal macOS binary ===\n");
run("npx tauri build --target universal-apple-darwin");

// ---- Locate build artifacts ----

const BUNDLE_DIR = join(
  TAURI_DIR,
  "target",
  "universal-apple-darwin",
  "release",
  "bundle"
);
const DMG_DIR = join(BUNDLE_DIR, "dmg");
const MACOS_DIR = join(BUNDLE_DIR, "macos");

// Find the DMG (filename may vary)
let dmgPath;
if (existsSync(DMG_DIR)) {
  const dmgFiles = readdirSync(DMG_DIR).filter((f) => f.endsWith(".dmg"));
  if (dmgFiles.length > 0) {
    dmgPath = join(DMG_DIR, dmgFiles[0]);
  }
}

if (!dmgPath || !existsSync(dmgPath)) {
  console.error("ERROR: DMG not found in", DMG_DIR);
  console.error(
    "Available files:",
    existsSync(DMG_DIR) ? readdirSync(DMG_DIR) : "directory missing"
  );
  process.exit(1);
}

console.log(`Found DMG: ${dmgPath}`);

// Find the .app.tar.gz (updater artifact) and its .sig
const tarGzName = `${PRODUCT}.app.tar.gz`;
const tarGzPath = join(MACOS_DIR, tarGzName);
const sigPath = `${tarGzPath}.sig`;

if (!existsSync(tarGzPath)) {
  console.error(`ERROR: Update tarball not found at ${tarGzPath}`);
  console.error(
    "Available files:",
    existsSync(MACOS_DIR) ? readdirSync(MACOS_DIR) : "directory missing"
  );
  process.exit(1);
}

if (!existsSync(sigPath)) {
  console.error(`ERROR: Signature not found at ${sigPath}`);
  process.exit(1);
}

// ---- Ad-hoc code sign ----

console.log("\n=== Ad-hoc code signing ===\n");

const appPath = join(MACOS_DIR, `${PRODUCT}.app`);
if (existsSync(appPath)) {
  run(`codesign --force --deep --sign - "${appPath}"`);
  console.log("Ad-hoc signed successfully.");
}

// ---- Generate latest.json ----

console.log("\n=== Generating latest.json ===\n");

const signature = readFileSync(sigPath, "utf-8").trim();
const pubDate = new Date().toISOString();
const downloadUrl = `https://github.com/mvarley07/SymbolSweep/releases/download/${TAG}/${tarGzName}`;

const latestJson = {
  version: TAG,
  notes: `${PRODUCT} ${TAG}`,
  pub_date: pubDate,
  platforms: {
    "darwin-aarch64": {
      signature: signature,
      url: downloadUrl,
    },
    "darwin-x86_64": {
      signature: signature,
      url: downloadUrl,
    },
  },
};

const latestJsonPath = join(BUNDLE_DIR, "latest.json");
writeFileSync(latestJsonPath, JSON.stringify(latestJson, null, 2));
console.log(`Generated ${latestJsonPath}`);

// ---- Create GitHub Release ----

console.log("\n=== Creating GitHub Release ===\n");

// Check if tag already exists
try {
  runCapture(`git rev-parse ${TAG}`);
  console.log(`Tag ${TAG} already exists.`);
} catch {
  run(`git tag ${TAG}`);
  run(`git push origin ${TAG}`);
}

// Check if release already exists
try {
  runCapture(`gh release view ${TAG}`);
  console.error(
    `ERROR: Release ${TAG} already exists. Bump the version in tauri.conf.json first.`
  );
  process.exit(1);
} catch {
  // Release doesn't exist, good
}

run(
  `gh release create ${TAG} --title "${PRODUCT} ${TAG}" --generate-notes "${dmgPath}" "${tarGzPath}" "${sigPath}" "${latestJsonPath}"`
);

console.log(`\n=== Release ${TAG} created successfully! ===`);
console.log(`\nGitHub Release:`);
console.log(
  `  https://github.com/mvarley07/SymbolSweep/releases/tag/${TAG}`
);
console.log(`\nDirect DMG download (for Gumroad):`);
console.log(
  `  https://github.com/mvarley07/SymbolSweep/releases/latest/download/${dmgPath.split("/").pop()}`
);
console.log(`\nUpdater endpoint:`);
console.log(
  `  https://github.com/mvarley07/SymbolSweep/releases/latest/download/latest.json`
);
