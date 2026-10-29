// Signs the Firefox add-on through addons.mozilla.org on the unlisted
// channel: Mozilla signs it so release Firefox installs it permanently, but
// it is not listed on the add-ons site. Run scripts/package-extensions.ps1
// first. The signed file is written to dist/extensions/downpour-firefox.xpi.
//
// Usage: node scripts/sign-firefox.mjs --credentials <path to mozilla.env>
//   (or set DOWNPOUR_AMO_CREDENTIALS to that path)
//
// The env file holds JWT_issuer=... and JWT_secret=... (AMO API keys). They
// reach web-ext only through environment variables, never the command line,
// so they do not show up in the process list or in this script's output.
// AMO accepts each version number once: raise "version" in
// extension/firefox/manifest.json for every new submission.

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourceDir = path.join(root, "dist", "extensions", "firefox");
const artifactsDir = path.join(root, "dist", "extensions", "signed");
const signedTarget = path.join(root, "dist", "extensions", "downpour-firefox.xpi");

const flag = process.argv.indexOf("--credentials");
const credentialsFile = flag >= 0 ? process.argv[flag + 1] : process.env.DOWNPOUR_AMO_CREDENTIALS;
if (!credentialsFile) {
  fail("Pass --credentials <mozilla.env> or set DOWNPOUR_AMO_CREDENTIALS.");
}
if (!fs.existsSync(path.join(sourceDir, "manifest.json"))) {
  fail(`${sourceDir} has no manifest.json; run scripts/package-extensions.ps1 first.`);
}

const credentials = readCredentials(credentialsFile);
const version = JSON.parse(fs.readFileSync(path.join(sourceDir, "manifest.json"), "utf-8")).version;
console.log(`Signing Downpour for Firefox ${version} (unlisted channel)…`);
fs.rmSync(artifactsDir, { recursive: true, force: true });

const webExtArguments = ["--yes", "web-ext@8", "sign", "--channel", "unlisted", "--source-dir", sourceDir, "--artifacts-dir", artifactsDir];
const options = {
  stdio: "inherit",
  env: { ...process.env, WEB_EXT_API_KEY: credentials.issuer, WEB_EXT_API_SECRET: credentials.secret },
};
// npx is a .cmd script on Windows, which only a shell can start. The shell
// gets one command line built here, with each argument quoted because the
// folders may contain spaces (Windows paths cannot contain a double quote).
// The name itself stays bare: a quoted "npx" makes npx.cmd look for npm in
// the current folder instead of its own.
const result =
  process.platform === "win32"
    ? spawnSync(["npx", ...webExtArguments.map((text) => `"${text}"`)].join(" "), { ...options, shell: true })
    : spawnSync("npx", webExtArguments, options);
if (result.error) {
  fail(`Could not start npx: ${result.error.message}. Is Node.js with npm on PATH?`);
}
if (result.status !== 0) {
  fail("Signing failed. If AMO says this version was already submitted, raise the version and package again.");
}

const signed = fs.readdirSync(artifactsDir).filter((name) => name.endsWith(".xpi"));
if (signed.length !== 1) {
  fail(`Expected one signed .xpi in ${artifactsDir}, found ${signed.length}.`);
}
fs.copyFileSync(path.join(artifactsDir, signed[0]), signedTarget);
console.log(`Signed add-on: ${signedTarget}`);

function readCredentials(file) {
  if (!fs.existsSync(file)) {
    fail(`Credentials file not found: ${file}`);
  }
  const values = {};
  for (const line of fs.readFileSync(file, "utf-8").split(/\r?\n/)) {
    const match = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$/);
    if (match) {
      values[match[1]] = match[2].replace(/^["']|["']$/g, "");
    }
  }
  const issuer = values.JWT_issuer ?? values.WEB_EXT_API_KEY;
  const secret = values.JWT_secret ?? values.WEB_EXT_API_SECRET;
  if (!issuer || !secret) {
    fail(`${file} must define JWT_issuer and JWT_secret.`);
  }
  return { issuer, secret };
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
