// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFile } from "node:child_process";
import { createServer } from "node:net";
import { promisify } from "node:util";
import { describe, expect, it } from "vitest";
import { identityFromSigningSeed } from "./identity.js";
import { acquireMissionWriter, missionIdentity, missionIdentitySeed } from "./mission-identity.js";

const exec = promisify(execFile);
const root = "a".repeat(64);

async function unusedPort(): Promise<number> {
  const server = createServer();
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("Missing lock address");
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

describe("Secret-backed mission identity", () => {
  it("matches RFC8032 Ed25519 and independently calculated canonical DID", () => {
    const seed = Buffer.from("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "hex");
    const identity = identityFromSigningSeed(seed);
    expect(identity.signingPublicKey.toString("hex"))
      .toBe("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    expect(identity.did).toBe("did:mesh:21fe31dfa154a261626bf854046fd227");
    expect(identityFromSigningSeed(new Uint8Array(seed))).toEqual(identity);
    seed.fill(0);
    expect(identity.signingPrivateKey.toString("hex"))
      .toBe("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
  });

  it.each([null, undefined, "a".repeat(32), [], new Uint8Array(31), new Uint8Array(33)])
    ("rejects an invalid raw seed: %s", seed => {
      expect(() => identityFromSigningSeed(seed as Uint8Array)).toThrow();
    });

  it("matches independent HMAC vectors and separates roles, owners and pod lifetimes", () => {
    const runtime = missionIdentitySeed(root, "runtime", "sandbox-uid", "pod-uid");
    const dispatcher = missionIdentitySeed(root, "dispatcher", "sandbox-uid", "pod-uid");
    expect(runtime.toString("hex")).toBe("0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf");
    expect(dispatcher.toString("hex")).toBe("e582df2d3650389264eba8d39d5f0076d881660ee6bf826364deebb2fabe36f0");
    const seeds = [runtime, dispatcher,
      missionIdentitySeed(root, "runtime", "other-owner", "pod-uid"),
      missionIdentitySeed(root, "runtime", "sandbox-uid", "new-pod"),
      missionIdentitySeed("b".repeat(64), "runtime", "sandbox-uid", "pod-uid"),
    ];
    expect(new Set(seeds.map(seed => seed.toString("hex"))).size).toBe(seeds.length);
    expect(missionIdentity(root, "runtime", "sandbox-uid", "pod-uid"))
      .toEqual(identityFromSigningSeed(runtime));
    expect(missionIdentitySeed(root.toUpperCase(), "runtime", "sandbox-uid", "pod-uid")).toEqual(runtime);
  });

  it.each([
    ["", "runtime", "owner", "pod"],
    ["a".repeat(63), "runtime", "owner", "pod"],
    ["g".repeat(64), "runtime", "owner", "pod"],
    [null, "runtime", "owner", "pod"],
    [root, "unknown", "owner", "pod"],
    [root, "runtime", "", "pod"],
    [root, "runtime", "owner", ""],
    [root, "runtime", "a/b", "pod"],
    [root, "runtime", "owner", "pod\n"],
    [root, "runtime", "a".repeat(254), "pod"],
  ])("rejects incomplete or malformed identity bindings", (...args) => {
    expect(() => missionIdentitySeed(...args as Parameters<typeof missionIdentitySeed>))
      .toThrow("Mission identity requires");
  });
});

describe("cooperative mission writer ownership", () => {
  it.each([0, -1, 65536, 1.1, NaN])("rejects invalid lock port %s", async port => {
    await expect(acquireMissionWriter(port)).rejects.toThrow("Invalid mission writer lock port");
  });

  it("excludes another process and releases only when its owner closes", async () => {
    const port = await unusedPort();
    const lock = await acquireMissionWriter(port);
    try {
      await expect(acquireMissionWriter(port)).rejects.toMatchObject({ code: "EADDRINUSE" });
      const child = await exec(process.execPath, ["--input-type=module", "-e", `
        import { createServer } from 'node:net';
        const server = createServer();
        server.on('error', error => { console.log(error.code); });
        server.listen({host: '127.0.0.1', port: Number(process.argv[1]), exclusive: true}, () => {
          console.log('acquired'); server.close();
        });
      `, String(port)], { timeout: 5000 });
      expect(child.stdout.trim()).toBe("EADDRINUSE");
    } finally {
      await Promise.all([lock.close(), lock.close()]);
    }
    const next = await acquireMissionWriter(port);
    await next.close();
  });
});
