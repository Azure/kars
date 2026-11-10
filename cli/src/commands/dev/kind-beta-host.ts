// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { X509Certificate } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import type { BetaExecute } from "./kind-beta.js";
import type { KindBetaState } from "./kind-beta-state.js";

export function ownedHostsContent(content: string, hostname: string, owner: string, add: boolean): string {
  if (!/^[a-z0-9-]+\.kars\.test$/.test(hostname) || !/^[a-f0-9-]{36}$/.test(owner)) throw new Error("Invalid owned hostname");
  const line = `127.0.0.1 ${hostname} # kars-kind-owner=${owner}`;
  const lines = content.split("\n");
  if (lines.some(value => value !== line && value.split("#")[0]!.trim().split(/\s+/).slice(1).includes(hostname))) {
    throw new Error("Hostname has a foreign hosts entry; nothing was changed");
  }
  if (add) return lines.includes(line) ? content : `${content}${content.endsWith("\n") ? "" : "\n"}${line}\n`;
  return lines.filter(value => value !== line).join("\n");
}
// Passed as a fixed program, not interpolated shell code. Arguments contain only the owned public hostname and nonce.
const hostsProgram = `const fs = require('node:fs');
const edit = ${ownedHostsContent.toString()};
const [host, owner, action] = process.argv.slice(1);
const file = '/etc/hosts', lock = '/etc/.kars-kind-hosts-lock', next = '/etc/.kars-kind-hosts-' + owner;
fs.mkdirSync(lock, {mode: 0o700});
try {
  const info = fs.lstatSync(file);
  if (!info.isFile() || info.isSymbolicLink()) throw Error('Hosts must be a regular file');
  const original = fs.readFileSync(file, 'utf8'), result = edit(original, host, owner, action === 'add');
  if (original !== result) {
    fs.writeFileSync(next, result, {flag:'wx', mode:info.mode & 0o777});
    fs.chownSync(next, info.uid, info.gid);
    const current = fs.lstatSync(file);
    if (current.ino !== info.ino || current.mtimeMs !== info.mtimeMs || fs.readFileSync(file,'utf8') !== original) throw Error('Hosts changed concurrently; retained staged change');
    fs.renameSync(next, file);
  }
} finally { fs.rmdirSync(lock); }`;

export function assertManagedHostSupport(platform = process.platform): void {
  if (platform !== "darwin" && !(platform === "linux" && existsSync("/usr/sbin/update-ca-certificates"))) {
    throw new Error("Interactive local Dex trust currently supports macOS login keychain or Linux update-ca-certificates. Use --beta-config <file> with external OIDC on other hosts.");
  }
}
export async function changeManagedHostTrust(state: KindBetaState, add: boolean, execute: BetaExecute, platform = process.platform): Promise<void> {
  assertManagedHostSupport(platform);
  const trust = state.trust;
  if (!trust) throw new Error("Missing owned CA receipt");
  const ca = new X509Certificate(readFileSync(trust.certificate));
  if (!ca.ca || ca.fingerprint256 !== trust.fingerprint || ca.subject !== "CN=Kars Kind CA") throw new Error("Owned CA fingerprint changed; host trust was not modified");
  ownedHostsContent("", trust.hostname, state.owner, add);
  const elevated = (tool: string, args: string[]) => execute("sudo", [tool, ...args], { stdio: "inherit" });
  const hosts = () => elevated(process.execPath, ["-e", hostsProgram, trust.hostname, state.owner, add ? "add" : "remove"]);
  if (add) await hosts();
  if (platform === "darwin") {
    const keychain = path.join(homedir(), "Library/Keychains/login.keychain-db");
    if (add) await execute("security", ["add-trusted-cert", "-r", "trustRoot", "-p", "ssl", "-s", trust.hostname, "-k", keychain, trust.certificate], { stdio: "inherit" });
    else {
      // Only this newly generated certificate is removed; never reset a keychain or trust domain.
      const installed = await execute("security", ["find-certificate", "-a", "-p", keychain], { stdio: "pipe" });
      const present = (installed.stdout.match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g) ?? [])
        .some(value => new X509Certificate(value).fingerprint256 === trust.fingerprint);
      if (present) {
        await execute("security", ["remove-trusted-cert", trust.certificate], { stdio: "inherit" });
        await execute("security", ["delete-certificate", "-Z", ca.fingerprint.replaceAll(":", ""), keychain], { stdio: "inherit" });
      }
    }
  } else {
    const target = `/usr/local/share/ca-certificates/kars-kind-${state.owner}.crt`;
    if (existsSync(target)) {
      if (new X509Certificate(readFileSync(target)).fingerprint256 !== trust.fingerprint) throw new Error("Foreign trust file; nothing overwritten");
    } else if (add) await elevated("install", ["-m", "0644", trust.certificate, target]);
    if (!add && existsSync(target)) await elevated("rm", ["--", target]);
    await elevated("/usr/sbin/update-ca-certificates", []);
  }
  if (!add) await hosts();
}
