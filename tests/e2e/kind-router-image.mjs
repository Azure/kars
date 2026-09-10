// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Kind's ctr --digests import creates import-date@digest, not repository@digest.
// CRI resolves the latter by exact reference, separately from its config-ID key.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";

export const TAG = "docker.io/library/kars-inference-router:e2e";
export const FIXTURE = "kars-inference-router:e2e";
const REPOSITORY = "docker.io/library/kars-inference-router";
const DIGEST = /^sha256:[a-f0-9]{64}$/;
const MANIFESTS = new Set([
  "application/vnd.oci.image.manifest.v1+json",
  "application/vnd.docker.distribution.manifest.v2+json",
]);
const INDEXES = new Set([
  "application/vnd.oci.image.index.v1+json",
  "application/vnd.docker.distribution.manifest.list.v2+json",
]);
const CONFIGS = new Set([
  "application/vnd.oci.image.config.v1+json",
  "application/vnd.docker.container.image.v1+json",
]);

function command(stage, binary, args) {
  try {
    return execFileSync(binary, args, {
      stdio: ["ignore", "pipe", "pipe"], timeout: 30_000, maxBuffer: 2 * 1024 * 1024,
    });
  } catch {
    throw new Error(`Budget image preflight ${stage} command failed`);
  }
}

export function imageRow(output, name) {
  const rows = output.toString("utf8").trim().split("\n")
    .map(line => line.trim().split(/\s+/)).filter(fields => fields[0] === name);
  assert(rows.length <= 1, "Ambiguous containerd image reference");
  if (!rows.length) return null;
  const [, mediaType, digest] = rows[0];
  assert(DIGEST.test(digest) && (MANIFESTS.has(mediaType) || INDEXES.has(mediaType)),
    "A real manifest/index target, not a config ID, is required");
  return { mediaType, digest };
}

export function checkedObject(bytes, digest) {
  assert(DIGEST.test(digest) && bytes.length <= 1024 * 1024, "Invalid image metadata bound");
  assert(`sha256:${createHash("sha256").update(bytes).digest("hex")}` === digest,
    "Image metadata content does not match its digest");
  return JSON.parse(bytes.toString("utf8"));
}

export function platformConfig(root, load, platform, mediaType = root.mediaType) {
  let manifest = root;
  assert(root.schemaVersion === 2 && (!root.mediaType || root.mediaType === mediaType), "Invalid manifest schema");
  if (INDEXES.has(mediaType)) {
    const selected = root.manifests?.filter(descriptor =>
      descriptor.platform?.os === platform.os && descriptor.platform?.architecture === platform.architecture);
    assert(selected?.length === 1, "Image platform is missing or ambiguous");
    assert(MANIFESTS.has(selected[0].mediaType), "Expected a platform manifest");
    mediaType = selected[0].mediaType;
    manifest = load(selected[0].digest);
    assert(manifest.schemaVersion === 2 && (!manifest.mediaType || manifest.mediaType === mediaType),
      "Platform manifest type differs from its descriptor");
  }
  assert(MANIFESTS.has(mediaType) && CONFIGS.has(manifest.config?.mediaType)
    && DIGEST.test(manifest.config?.digest) && Array.isArray(manifest.layers)
    && manifest.layers.every(layer => DIGEST.test(layer.digest)), "Invalid platform content graph");
  const config = load(manifest.config.digest);
  assert(config.os === platform.os && config.architecture === platform.architecture
    && Array.isArray(config.rootfs?.diff_ids) && config.rootfs.diff_ids.length === manifest.layers.length,
  "Image config does not cover the node platform and layers");
  return manifest.config.digest;
}

function completeImage(output, name, digest) {
  const row = output.toString("utf8").trim().split("\n").map(line => line.trim().split(/\s+/))
    .find(fields => fields[0] === name);
  assert(row && row[2] === digest && row[3] === "complete" && row.at(-1) === "true",
    "Platform image content must be complete and unpacked");
}

export async function qualifyNodes({ nodes, platformFor, expectedConfig, run, report, pause = ms =>
  new Promise(resolve => setTimeout(resolve, ms)) }) {
  assert(DIGEST.test(expectedConfig) && nodes.length > 0
    && new Set(nodes).size === nodes.length
    && nodes.every(name => /^kars-e2e-(?:control-plane|worker)(?:\d+)?$/.test(name)),
  "Only exact owned Kind nodes and the built image config may be used");
  let manifestDigest;
  const proofs = [];
  for (const node of nodes) {
    const platform = platformFor(node);
    assert(platform.os === "linux" && ["amd64", "arm64"].includes(platform.architecture),
      "Unsupported Kind fixture platform");
    const ctr = (...args) => run("docker", ["exec", node, "ctr", "-n", "k8s.io", ...args]);
    const cri = (...args) => JSON.parse(run("docker", ["exec", node, "crictl", ...args]).toString("utf8"));
    const filter = name => `name==${JSON.stringify(name)}`;
    const source = imageRow(ctr("images", "list", filter(TAG)), TAG);
    assert(source, "The existing loaded router tag is missing");
    assert(source.digest !== expectedConfig, "An image config ID cannot be an enforcement manifest");
    if (manifestDigest === undefined) manifestDigest = source.digest;
    assert(source.digest === manifestDigest, "Kind nodes contain different router manifests");
    const load = digest => checkedObject(ctr("content", "get", digest), digest);
    const root = load(source.digest);
    const configDigest = platformConfig(root, load, platform, source.mediaType);
    assert(configDigest === expectedConfig, "Loaded router is not the same image built by this fixture");
    completeImage(ctr("images", "check", "--snapshotter", "overlayfs", filter(TAG)), TAG, source.digest);
    const tagStatus = cri("inspecti", "--output", "json", TAG).status;
    assert(tagStatus?.id === configDigest, "CRI tag does not resolve the verified platform config");
    const canonical = `${REPOSITORY}@${source.digest}`;
    const exactReference = `${FIXTURE}@${source.digest}`;
    const before = imageRow(ctr("images", "list", filter(canonical)), canonical);
    if (before) assert(before.digest === source.digest && before.mediaType === source.mediaType,
      "Refusing to overwrite a canonical alias that targets another image");
    const criBefore = cri("images", "--output", "json").images;
    assert(Array.isArray(criBefore), "CRI image inventory is unavailable");
    const listed = criBefore.filter(image => image.id === configDigest);
    assert(listed.length === 1 && listed[0].repoTags?.includes(TAG), "CRI loaded tag identity differs");
    const digestVisibleBefore = listed[0].repoDigests?.includes(canonical) === true;
    report({ phase: "before", node, reference: exactReference, canonical, manifestDigest: source.digest,
      configDigest, platform: `${platform.os}/${platform.architecture}`,
      aliasPresent: before !== null, criDigestPresent: digestVisibleBefore, contentComplete: true });
    if (!before) {
      // Only add a new metadata reference to this same already-verified target.
      // Never force-replace a name, pull another image, or edit any content.
      ctr("images", "tag", TAG, canonical);
    }
    const after = imageRow(ctr("images", "list", filter(canonical)), canonical);
    assert(after?.digest === source.digest && after.mediaType === source.mediaType,
      "Canonical alias did not retain the exact manifest target");
    completeImage(ctr("images", "check", "--snapshotter", "overlayfs", filter(canonical)),
      canonical, source.digest);
    let visible = false;
    for (let attempt = 0; attempt < 20; attempt++) {
      const images = cri("images", "--output", "json").images;
      assert(Array.isArray(images), "CRI image inventory became unavailable");
      visible = images.some(image => image.id === configDigest && image.repoDigests?.includes(canonical));
      if (visible) break;
      await pause(500);
    }
    assert(visible, "CRI did not observe the canonical same-image alias within its bound");
    for (const reference of [canonical, exactReference]) {
      const status = cri("inspecti", "--output", "json", reference).status;
      assert(status?.id === configDigest && status.repoDigests?.includes(canonical),
        "The exact digest-qualified reference does not resolve through CRI");
    }
    const proof = { phase: "verified", node, reference: exactReference, canonical,
      manifestDigest: source.digest, configDigest, platform: `${platform.os}/${platform.architecture}`,
      aliasCreated: before === null, criResolved: true, contentComplete: true };
    report(proof);
    proofs.push(proof);
  }
  return { manifestDigest, reference: `${FIXTURE}@${manifestDigest}`, proofs };
}

export async function prepareRouterImage({ nodes, kube, values, report }) {
  const deadline = Date.now() + 90_000;
  if (process.env.KARS_STANDALONE_CLUSTER_UID) {
    const uid = kube(["get", "namespace", "kube-system", "-o", "jsonpath={.metadata.uid}"]).trim();
    assert(uid === process.env.KARS_STANDALONE_CLUSTER_UID, "Owned standalone cluster identity changed");
  }
  assert(values.inferenceRouter?.image?.repository === "kars-inference-router"
    && values.inferenceRouter.image.tag === "e2e"
    && !(values.controller?.extraEnv ?? []).some(entry => entry.name === "INFERENCE_ROUTER_IMAGE"),
  "Only the unchanged loaded router fixture reference is allowed");
  const expectedConfig = command("built-image-identity", "docker",
    ["image", "inspect", "--format", "{{.Id}}", FIXTURE]).toString("utf8").trim();
  const metadata = JSON.parse(kube(["get", "nodes", "-o", "json"]));
  const byName = new Map(metadata.items.map(node => [node.metadata.name, node]));
  for (const node of nodes) {
    assert(byName.get(node)?.metadata?.uid, "Kind node is not in the current Kubernetes cluster");
    const cluster = command("node-ownership", "docker", ["inspect", "--format",
      "{{index .Config.Labels \"io.x-k8s.kind.cluster\"}}", node]).toString("utf8").trim();
    assert(cluster === "kars-e2e", "Refusing a node outside the owned Kind fixture");
  }
  return qualifyNodes({
    nodes, expectedConfig, report,
    platformFor: node => ({ os: byName.get(node).status.nodeInfo.operatingSystem,
      architecture: byName.get(node).status.nodeInfo.architecture }),
    run: (binary, args) => {
      assert(Date.now() < deadline, "Budget image preflight exceeded its total bound");
      const operation = args[2] === "ctr" ? `${args[5]}-${args[6]}` : `cri-${args[3]}`;
      assert(["images-list", "images-check", "images-tag", "content-get", "cri-images", "cri-inspecti"].includes(operation),
        "Unexpected image fixture operation");
      return command(operation, binary, args);
    },
  });
}
