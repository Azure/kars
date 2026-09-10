// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";

const { TAG, FIXTURE, imageRow, checkedObject, platformConfig, qualifyNodes } = await import(
  new URL("../../../tests/e2e/kind-router-image.mjs", import.meta.url).href
);
const manifestType = "application/vnd.oci.image.manifest.v1+json";
const configType = "application/vnd.oci.image.config.v1+json";
const indexType = "application/vnd.oci.image.index.v1+json";
const PRIVATE = "DO-NOT-EXPORT-CONFIG-ENV-OR-ARGS";
const nodes = ["kars-e2e-control-plane", "kars-e2e-worker"];

function blob(value: unknown) {
  const bytes = Buffer.from(JSON.stringify(value));
  return { bytes, digest: `sha256:${createHash("sha256").update(bytes).digest("hex")}` };
}

function fixture(options: Record<string, boolean> = {}) {
  const config = blob({ os: "linux", architecture: "amd64", config: { Env: [PRIVATE], Cmd: [PRIVATE] },
    rootfs: { type: "layers", diff_ids: [`sha256:${"1".repeat(64)}`] } });
  const manifest = blob({ schemaVersion: 2, mediaType: manifestType,
    config: { mediaType: configType, digest: config.digest, size: config.bytes.length },
    layers: [{ mediaType: "application/vnd.oci.image.layer.v1.tar+gzip", digest: `sha256:${"2".repeat(64)}`, size: 100 }] });
  const index = blob({ schemaVersion: 2, mediaType: indexType, manifests: [{
    mediaType: manifestType, digest: manifest.digest, size: manifest.bytes.length,
    platform: { os: "linux", architecture: "amd64" },
  }] });
  const target = options.index ? index : manifest;
  const mediaType = options.index ? indexType : manifestType;
  const canonical = `docker.io/library/kars-inference-router@${target.digest}`;
  const aliases = new Map(nodes.map(node => [node, options.existing === true]));
  const calls: string[][] = [];
  const blobs = new Map([config, manifest, index].map(value => [value.digest, value.bytes]));
  const rows = (node: string) => [
    `REF TYPE DIGEST SIZE PLATFORMS LABELS`,
    ...(options.missingNode && node.endsWith("worker") ? [] : [
      `${TAG} ${mediaType} ${options.configTarget ? config.digest :
        options.differentNode && node.endsWith("worker") ? index.digest : target.digest} 100 B linux/amd64 -`]),
    `import-date@${target.digest} ${mediaType} ${target.digest} 100 B linux/amd64 -`,
    ...(aliases.get(node) ? [`${canonical} ${mediaType} ${options.foreignAlias ? config.digest : target.digest} 100 B linux/amd64 -`] : []),
  ].join("\n");
  const run = (_binary: string, args: string[]) => {
    calls.push(args);
    const node = args[1];
    if (args[2] === "ctr") {
      const action = args.slice(5);
      if (action[0] === "images" && action[1] === "list") return Buffer.from(rows(node));
      if (action[0] === "content" && action[1] === "get") {
        const bytes = blobs.get(action[2]);
        if (!bytes) throw new Error("Missing metadata fixture");
        return options.tamper ? Buffer.concat([bytes, Buffer.from(" ")]) : bytes;
      }
      if (action[0] === "images" && action[1] === "check") {
        const name = JSON.parse(action.at(-1)!.replace("name==", ""));
        return Buffer.from(`REF TYPE DIGEST STATUS SIZE UNPACKED\n${name} ${mediaType} ${target.digest} ${options.incomplete ? "incomplete" : "complete"} (3/3) 100B/100B ${!options.notUnpacked}\n`);
      }
      if (action[0] === "images" && action[1] === "tag") {
        expect(action).toEqual(["images", "tag", TAG, canonical]);
        aliases.set(node, true);
        return Buffer.from(canonical);
      }
    }
    if (args[2] === "crictl") {
      const ref = args.at(-1)!;
      const visible = aliases.get(node) && !options.noCriEvent;
      const status = { id: options.wrongConfig && args[3] === "inspecti" && ref !== TAG ? `sha256:${"4".repeat(64)}` : config.digest,
        repoTags: [TAG], repoDigests: visible ? [canonical] : [] };
      if (args[3] === "images") return Buffer.from(JSON.stringify({ images: [status] }));
      if (args[3] === "inspecti") return Buffer.from(JSON.stringify({ status,
        info: { imageSpec: { config: { Env: [PRIVATE] } } } }));
    }
    throw new Error("Unexpected fixture command");
  };
  const optionsFor = (report: (value: unknown) => void) => ({
    nodes, expectedConfig: config.digest, run, report, pause: async () => {},
    platformFor: () => ({ os: "linux", architecture: options.wrongPlatform ? "arm64" : "amd64" }),
  });
  return { config, manifest, index, target, canonical, calls, run, optionsFor };
}

describe("same-image Kind CRI preload fixture (unit orchestration, not native evidence)", () => {
  it("proves missing canonical references and aliases the identical manifest on every node", async () => {
    const f = fixture();
    const proofs: any[] = [];
    const result = await qualifyNodes(f.optionsFor(value => proofs.push(value)));
    expect(result.manifestDigest).toBe(f.target.digest);
    expect(result.manifestDigest).not.toBe(f.config.digest);
    expect(result.reference).toBe(`${FIXTURE}@${f.target.digest}`);
    expect(proofs.filter(proof => proof.phase === "before").every(
      proof => !proof.aliasPresent && !proof.criDigestPresent)).toBe(true);
    expect(proofs.filter(proof => proof.phase === "verified")).toHaveLength(2);
    expect(f.calls.filter(args => args.includes("tag"))).toHaveLength(2);
    expect(f.calls.some(args => args.includes("--force") || args.includes("pull") || args.includes("delete"))).toBe(false);
    expect(f.calls.filter(args => args.at(-1) === result.reference)).toHaveLength(2);
    expect(JSON.stringify(proofs)).not.toContain(PRIVATE);
  });

  it("checks index platform content and preserves an already-correct alias without rewriting", async () => {
    const f = fixture({ index: true, existing: true });
    const result = await qualifyNodes(f.optionsFor(() => {}));
    expect(result.manifestDigest).toBe(f.index.digest);
    expect(f.calls.some(args => args.includes("tag"))).toBe(false);
  });

  it.each(["configTarget", "tamper", "wrongPlatform", "incomplete", "notUnpacked"])(
    "rejects %s before adding any reference", async fault => {
      const f = fixture({ [fault]: true });
      await expect(qualifyNodes(f.optionsFor(() => {}))).rejects.toThrow();
      expect(f.calls.some(args => args.includes("tag"))).toBe(false);
    },
  );

  it("refuses a conflicting existing canonical alias rather than force-overwriting it", async () => {
    const f = fixture({ existing: true, foreignAlias: true });
    await expect(qualifyNodes(f.optionsFor(() => {}))).rejects.toThrow("another image");
    expect(f.calls.some(args => args.includes("tag"))).toBe(false);
  });

  it.each(["noCriEvent", "wrongConfig"])("does not qualify %s after metadata tagging", async fault => {
    const f = fixture({ [fault]: true });
    await expect(qualifyNodes(f.optionsFor(() => {}))).rejects.toThrow();
  });

  it.each(["missingNode", "differentNode"])("requires the same complete image on every node: %s", async fault => {
    const f = fixture({ [fault]: true });
    await expect(qualifyNodes(f.optionsFor(() => {}))).rejects.toThrow();
  });

  it("rejects arbitrary image references, config descriptors and mismatched metadata hashes", () => {
    expect(imageRow(`other ${manifestType} sha256:${"1".repeat(64)}`, TAG)).toBeNull();
    expect(() => imageRow(`${TAG} ${configType} sha256:${"1".repeat(64)}`, TAG)).toThrow();
    expect(() => checkedObject(Buffer.from("{}"), `sha256:${"1".repeat(64)}`)).toThrow();
  });

  it("rejects ambiguous platform descriptors instead of choosing an arbitrary image", () => {
    const f = fixture({ index: true });
    const value = JSON.parse(f.index.bytes.toString());
    value.manifests.push(structuredClone(value.manifests[0]));
    expect(() => platformConfig(value, () => ({}), { os: "linux", architecture: "amd64" })).toThrow("ambiguous");
  });
});
