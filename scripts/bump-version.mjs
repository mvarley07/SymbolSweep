#!/usr/bin/env node

/**
 * Bump version across all config files.
 *
 * Usage:
 *   node scripts/bump-version.mjs patch    # 1.0.0 -> 1.0.1
 *   node scripts/bump-version.mjs minor    # 1.0.0 -> 1.1.0
 *   node scripts/bump-version.mjs major    # 1.0.0 -> 2.0.0
 *   node scripts/bump-version.mjs 1.2.3    # set explicitly
 */

import { readFileSync, writeFileSync } from "fs";
import { join } from "path";
import { fileURLToPath } from "url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));
const ROOT = join(__dirname, "..");

const FILES = [
  join(ROOT, "package.json"),
  join(ROOT, "src-tauri", "tauri.conf.json"),
  join(ROOT, "src-tauri", "Cargo.toml"),
];

// Read current version from tauri.conf.json (source of truth)
const tauriConf = JSON.parse(
  readFileSync(join(ROOT, "src-tauri", "tauri.conf.json"), "utf-8")
);
const current = tauriConf.version;
const [major, minor, patch] = current.split(".").map(Number);

// Determine new version
const arg = process.argv[2];
if (!arg) {
  console.error("Usage: node scripts/bump-version.mjs <patch|minor|major|X.Y.Z>");
  process.exit(1);
}

let next;
if (arg === "patch") next = `${major}.${minor}.${patch + 1}`;
else if (arg === "minor") next = `${major}.${minor + 1}.0`;
else if (arg === "major") next = `${major + 1}.0.0`;
else if (/^\d+\.\d+\.\d+$/.test(arg)) next = arg;
else {
  console.error(`Invalid argument: ${arg}`);
  console.error("Expected: patch, minor, major, or X.Y.Z");
  process.exit(1);
}

if (next === current) {
  console.log(`Already at ${current}, nothing to do.`);
  process.exit(0);
}

console.log(`${current} -> ${next}\n`);

for (const filepath of FILES) {
  const filename = filepath.replace(ROOT + "/", "");
  let content = readFileSync(filepath, "utf-8");

  if (filepath.endsWith(".toml")) {
    // Cargo.toml: replace version = "X.Y.Z" (only the first occurrence in [package])
    content = content.replace(
      /^(version\s*=\s*")([^"]+)(")/m,
      `$1${next}$3`
    );
  } else {
    // JSON files: parse, update, write back
    const json = JSON.parse(content);
    json.version = next;
    content = JSON.stringify(json, null, 2) + "\n";
  }

  writeFileSync(filepath, content);
  console.log(`  updated ${filename}`);
}

console.log(`\nVersion bumped to ${next}. Now commit and run: npm run release`);
