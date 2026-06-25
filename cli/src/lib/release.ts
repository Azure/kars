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
