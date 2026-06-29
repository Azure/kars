// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// lib/release.ts — published-release helpers shared by `kars up --release`,
// `kars dev --release`, and `kars upgrade`: the canonical GHCR image plan,
// semantic-version comparison, and latest-release discovery.

export const RELEASE_GHCR = "ghcr.io/azure";

export interface ReleaseImage {
  /** Source image on public GHCR (version-tagged). */
  src: string;
  /** Target repo:tag inside the user's ACR. */
  target: string;
  /** Required images abort the operation on import failure. */
  required: boolean;
}

/**
 * The canonical set of public release images to import into a user's ACR for a
 * given version, re-tagged to the `:latest` names the Helm chart + agentmesh
 * manifest reference. When `tagAlso` is set, each image is ALSO imported under
 * that immutable tag (e.g. the version) so a deploy can be pinned / rolled back.
 */
export function releaseImagePlan(
  version: string,
  opts: { includeRuntimes?: boolean } = {},
): ReleaseImage[] {
  const G = RELEASE_GHCR;
  const images: ReleaseImage[] = [
    { src: `${G}/kars-controller:${version}`, target: "kars-controller:latest", required: true },
    { src: `${G}/kars-inference-router:${version}`, target: "kars-inference-router:latest", required: true },
    { src: `${G}/openclaw-sandbox:${version}`, target: "openclaw-sandbox:latest", required: true },
    // GHCR publishes `kars-agentmesh-*`; the manifest references
    // `agentmesh-*-agt:latest`, so re-tag on import.
    { src: `${G}/kars-agentmesh-relay:${version}`, target: "agentmesh-relay-agt:latest", required: true },
    { src: `${G}/kars-agentmesh-registry:${version}`, target: "agentmesh-registry-agt:latest", required: true },
  ];
  if (opts.includeRuntimes !== false) {
    for (const rt of [
      "kars-runtime-openai-agents", "kars-runtime-maf-python", "kars-runtime-anthropic",
      "kars-runtime-langgraph", "kars-runtime-langgraph-ts", "kars-runtime-pydantic-ai",
      "kars-runtime-hermes",
    ]) {
      images.push({ src: `${G}/${rt}:${version}`, target: `${rt}:latest`, required: false });
    }
  }
  return images;
}

export interface ParsedVersion {
  major: number;
  minor: number;
  patch: number;
  /** Pre-release identifiers (e.g. ["interim", "3"]) — empty for a stable tag. */
  pre: string[];
  /** Original string. */
  raw: string;
}

/** Parse a `vMAJOR.MINOR.PATCH[-pre.N]` tag (leading `v` optional). */
export function parseVersionTag(tag: string): ParsedVersion | null {
  const m = tag.trim().match(/^v?(\d+)\.(\d+)\.(\d+)(?:-(.+))?$/);
  if (!m) return null;
  return {
    major: parseInt(m[1], 10),
    minor: parseInt(m[2], 10),
    patch: parseInt(m[3], 10),
    pre: m[4] ? m[4].split(".") : [],
    raw: tag.trim(),
  };
}

/**
 * Compare two version tags. Returns <0 if a<b, 0 if equal, >0 if a>b.
 * Semantics follow SemVer: a stable release outranks a pre-release of the same
 * MAJOR.MINOR.PATCH; pre-release identifiers compare left-to-right (numeric
 * parts numerically, otherwise lexically). Unparseable tags sort lowest but are
 * still deterministically ordered by their raw string.
 */
export function compareVersions(a: string, b: string): number {
  const pa = parseVersionTag(a);
  const pb = parseVersionTag(b);
  if (!pa && !pb) return a < b ? -1 : a > b ? 1 : 0;
  if (!pa) return -1;
  if (!pb) return 1;
  for (const k of ["major", "minor", "patch"] as const) {
    if (pa[k] !== pb[k]) return pa[k] < pb[k] ? -1 : 1;
  }
  // Stable (no pre) outranks pre-release.
  if (pa.pre.length === 0 && pb.pre.length > 0) return 1;
  if (pa.pre.length > 0 && pb.pre.length === 0) return -1;
  const n = Math.max(pa.pre.length, pb.pre.length);
  for (let i = 0; i < n; i++) {
    const x = pa.pre[i];
    const y = pb.pre[i];
    if (x === undefined) return -1; // shorter pre-release is lower
    if (y === undefined) return 1;
    const xn = /^\d+$/.test(x);
    const yn = /^\d+$/.test(y);
    if (xn && yn) {
      const dx = parseInt(x, 10), dy = parseInt(y, 10);
      if (dx !== dy) return dx < dy ? -1 : 1;
    } else if (x !== y) {
      return x < y ? -1 : 1;
    }
  }
  return 0;
}

/**
 * Fetch the latest published release tag for Azure/kars from the GitHub API.
 * Returns the tag (e.g. "v0.1.16") or null when offline / rate-limited / no
 * release. Never throws.
 */
export async function fetchLatestReleaseTag(
  fetchImpl: typeof fetch = fetch,
): Promise<string | null> {
  try {
    const res = await fetchImpl(
      "https://api.github.com/repos/Azure/kars/releases/latest",
      { headers: { Accept: "application/vnd.github+json", "User-Agent": "kars-cli" } },
    );
    if (!res.ok) return null;
    const body = (await res.json()) as { tag_name?: string };
    return body.tag_name ?? null;
  } catch {
    return null;
  }
}

export interface ReleaseNote {
  tag: string;
  name: string;
  /** Raw release body (markdown). */
  body: string;
}

/**
 * Fetch recent published releases (newest first). Used for the changelog
 * summary and the image-digest version fallback. Never throws.
 */
export async function fetchRecentReleases(
  limit = 20,
  fetchImpl: typeof fetch = fetch,
): Promise<ReleaseNote[]> {
  try {
    const res = await fetchImpl(
      `https://api.github.com/repos/Azure/kars/releases?per_page=${limit}`,
      { headers: { Accept: "application/vnd.github+json", "User-Agent": "kars-cli" } },
    );
    if (!res.ok) return [];
    const body = (await res.json()) as Array<{ tag_name?: string; name?: string; body?: string }>;
    return body
      .filter((r) => r.tag_name)
      .map((r) => ({ tag: r.tag_name as string, name: r.name || (r.tag_name as string), body: r.body || "" }));
  } catch {
    return [];
  }
}

/**
 * The set of releases strictly newer than `current` and up to (and including)
 * `target`, oldest→newest — i.e. exactly what an upgrade would apply. Used for
 * the changelog summary.
 */
export function releasesBetween(
  releases: ReleaseNote[],
  current: string,
  target: string,
): ReleaseNote[] {
  return releases
    .filter((r) => {
      const gtCurrent = current ? compareVersions(r.tag, current) > 0 : true;
      const leTarget = compareVersions(r.tag, target) <= 0;
      return gtCurrent && leTarget;
    })
    .sort((a, b) => compareVersions(a.tag, b.tag));
}

/**
 * Fetch the annotated tag message for a release tag — this carries the real,
 * human-written changelog (feature bullets) for kars releases, unlike the
 * auto-generated release body. Returns null when the tag is lightweight /
 * unreachable. Never throws.
 */
export async function fetchTagMessage(
  tag: string,
  fetchImpl: typeof fetch = fetch,
): Promise<string | null> {
  try {
    const refRes = await fetchImpl(
      `https://api.github.com/repos/Azure/kars/git/refs/tags/${tag}`,
      { headers: { Accept: "application/vnd.github+json", "User-Agent": "kars-cli" } },
    );
    if (!refRes.ok) return null;
    const ref = (await refRes.json()) as { object?: { sha?: string; type?: string } };
    // Lightweight tags point straight at a commit (no annotation message).
    if (ref.object?.type !== "tag" || !ref.object.sha) return null;
    const tagRes = await fetchImpl(
      `https://api.github.com/repos/Azure/kars/git/tags/${ref.object.sha}`,
      { headers: { Accept: "application/vnd.github+json", "User-Agent": "kars-cli" } },
    );
    if (!tagRes.ok) return null;
    return ((await tagRes.json()) as { message?: string }).message ?? null;
  } catch {
    return null;
  }
}

/** Anonymous GHCR pull token for a public repo (e.g. "azure/kars-controller"). */
async function ghcrToken(repo: string, fetchImpl: typeof fetch): Promise<string | null> {
  try {
    const res = await fetchImpl(`https://ghcr.io/token?scope=repository:${repo}:pull`, {
      headers: { "User-Agent": "kars-cli" },
    });
    if (!res.ok) return null;
    return ((await res.json()) as { token?: string }).token ?? null;
  } catch {
    return null;
  }
}

/**
 * Collect every manifest digest (the multi-arch index digest plus each per-arch
 * sub-manifest digest) for `ghcr.io/azure/<repo>:<tag>`. A running pod's
 * `imageID` is a per-arch digest, while `:latest` resolves to the index digest —
 * gathering both lets a caller match either. Digests are content-addressed, so
 * GHCR and an `az acr import`-copied ACR share identical values. Never throws.
 */
export async function ghcrManifestDigests(
  repo: string,
  tag: string,
  fetchImpl: typeof fetch = fetch,
): Promise<Set<string>> {
  const out = new Set<string>();
  const token = await ghcrToken(repo, fetchImpl);
  if (!token) return out;
  const accept = [
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
  ].join(", ");
  try {
    const res = await fetchImpl(`https://ghcr.io/v2/${repo}/manifests/${tag}`, {
      headers: { Authorization: `Bearer ${token}`, Accept: accept, "User-Agent": "kars-cli" },
    });
    if (!res.ok) return out;
    const indexDigest = res.headers.get("docker-content-digest");
    if (indexDigest) out.add(indexDigest);
    const body = (await res.json()) as { manifests?: Array<{ digest?: string }> };
    for (const m of body.manifests ?? []) {
      if (m.digest) out.add(m.digest);
    }
  } catch {
    /* ignore — best-effort */
  }
  return out;
}
