#!/usr/bin/env node
// Bump the version everywhere it is written down.
//
//   node scripts/set_version.js 0.2.0
//   node scripts/set_version.js 0.2.0-beta.1
//
// The version lives in four files and they must agree: the release workflow
// refuses to build when the tag and tauri.conf.json disagree, and an app that
// reports a different version than its own installer is worse than one that
// reports none — a bug report then cites a build that never existed.
//
// ⚠️ SemVer, not the Python-style `0.2.0b1` BamDude uses. Cargo will not parse
// that spelling at all, so this repo writes `-beta.1` and tags the same string.
// One representation, no mapping layer to drift.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];

if (!version) {
  console.error("usage: node scripts/set_version.js <version>");
  process.exit(1);
}

// Deliberately strict. A typo here propagates into a tag, and tags are
// immutable — the repair costs a version number.
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error(`not a SemVer version: ${version}`);
  console.error("expected e.g. 0.2.0 or 0.2.0-beta.1");
  process.exit(1);
}

/** Replaces exactly one match, and fails loudly when the shape moved. */
function edit(relative, pattern, replacement) {
  const path = join(root, relative);
  const before = readFileSync(path, "utf8");
  const matches = before.match(pattern);

  if (!matches) {
    console.error(`${relative}: no version field matched — has the file changed shape?`);
    process.exit(1);
  }

  const after = before.replace(pattern, replacement);
  writeFileSync(path, after);
  console.log(`  ${relative}`);
}

console.log(`setting version ${version} in:`);

edit("package.json", /("version":\s*")[^"]+(")/, `$1${version}$2`);
edit("src-tauri/tauri.conf.json", /("version":\s*")[^"]+(")/, `$1${version}$2`);

// Anchored on [package] so a dependency's version can never be hit instead.
edit("src-tauri/Cargo.toml", /(\[package\][\s\S]*?\nversion = ")[^"]+(")/, `$1${version}$2`);

// The lockfile carries our own package too, and leaving it stale turns every
// later build into a spurious diff.
edit(
  "src-tauri/Cargo.lock",
  /(name = "bamdude-bridge"\nversion = ")[^"]+(")/,
  `$1${version}$2`,
);

console.log("\nNext: commit, land on dev, wait for CI, then tag.");
console.log("See CONTRIBUTING.md — a stable tag must sit on main.");
