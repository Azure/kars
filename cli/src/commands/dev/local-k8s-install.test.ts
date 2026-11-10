// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { createHash } from "node:crypto";
import { buildLocalConformanceRunnerImage, ensureCluster, installLocalCoreChart, loadRequiredLocalImages, localK8sImagePlan } from "./local-k8s.js";
import { schemaFixture } from "../../lib/schema-stage.test-support.js";
import type { SchemaExecute } from "../../lib/schema-documents.js";

const { execute, stageRust, publishCore } = vi.hoisted(() => ({ execute: vi.fn<SchemaExecute>(), stageRust: vi.fn(), publishCore: vi.fn() }));
vi.mock("../../lib/private-core-publication.js", () => ({ publishPrivateCore: publishCore }));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("../../lib/stage-rust-bin.js", () => ({ stageRustBinaries: stageRust }));
afterEach(() => {
  vi.restoreAllMocks();
  execute.mockReset();
  stageRust.mockReset();
  publishCore.mockReset();
});

describe("existing kind core installation target", () => {
  it("runs original cold beta hooks directly with dedicated kubeconfig, never compatibility staging", async () => {
    execute.mockResolvedValue({ stdout: "" });
    await installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/bundled/core", ["/reviewed/core.yaml"], [], undefined, { kubeconfig: "/dedicated/config" });
    expect(execute).toHaveBeenCalledExactlyOnceWith("/tools/helm", ["--kubeconfig", "/dedicated/config", "--kube-context", "kind-chosen", "install", "kars", "/bundled/core", "--namespace", "kars-system", "--timeout", "20m", "-f", "/reviewed/core.yaml", "--wait", "--wait-for-jobs"], { stdio: "inherit" });
    expect(publishCore).not.toHaveBeenCalled();
  });

  it("pins every existing-cluster operation and preserves the explicit fenced SSA upgrade", async () => {
    const f = schemaFixture(); f.options.releaseExists = true;
    execute.mockImplementation(async (file, args, options) => {
      expect(args.slice(0, 4)).toEqual(["--kubeconfig", "/dedicated/config", file === "/tools/helm" ? "--kube-context" : "--context", "kind-chosen"]);
      return f.execute(file === "/tools/helm" ? "helm" : "kubectl", args.slice(4), options);
    });
    await installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart", [], [], undefined, undefined,
      { kubeconfig: "/dedicated/config", fencedControllerNoop: true });
    const upgrade = execute.mock.calls.find(([, args]) => args[4] === "upgrade");
    expect(upgrade?.[1]).toContain("--server-side=true");
    expect(publishCore).not.toHaveBeenCalled();
  });

  it("rejects contradictory cold and existing-cluster modes before any operation", async () => {
    await expect(installLocalCoreChart("helm", "kubectl", "chosen", "kars", "/chart", [], [], undefined,
      { kubeconfig: "/cold" }, { kubeconfig: "/existing" })).rejects.toThrow("not both");
    expect(execute).not.toHaveBeenCalled();
  });

  it("refuses cold beta against an existing cluster before any mutation", async () => {
    execute.mockResolvedValue({ stdout: "chosen\n" });
    await expect(ensureCluster("kind", "chosen", {}, "docker", { config: "kind.yaml", kubeconfig: "dedicated", image: "reviewed" })).rejects.toThrow(/fresh cluster/);
    expect(execute).toHaveBeenCalledExactlyOnceWith("kind", ["get", "clusters"], { env: {} });
  });

  it("does not treat failed cluster inventory as permission to create", async () => {
    execute.mockRejectedValue(new Error("runtime inventory unavailable"));
    await expect(ensureCluster("kind", "chosen", {}, "docker", { config: "kind.yaml", kubeconfig: "dedicated", image: "reviewed" })).rejects.toThrow(/inventory unavailable/);
    expect(execute).toHaveBeenCalledExactlyOnceWith("kind", ["get", "clusters"], { env: {} });
  });

  it.each([false, true])("captures managed custody immediately after Kind creation, including retained failure (%s)", async failed => {
    const order: string[] = [];
    execute.mockImplementation(async (_file, args, options) => {
      order.push(args.join(" "));
      if (failed && args[0] === "create") throw new Error("retained create failure");
      if (args[0] === "inspect") return { stdout: `${"a".repeat(64)} chosen` };
      if (args[0] === "exec") return { stdout: String(options?.input ?? "") };
      return { stdout: args[0] === "get" && args[1] === "clusters" ? "" : "chosen-control-plane" };
    });
    const attempt = ensureCluster("kind", "chosen", {}, "docker", {
      config: "kind.yaml", kubeconfig: "dedicated", image: "reviewed", onCreated: async () => { order.push("capture custody"); },
    });
    if (failed) await expect(attempt).rejects.toThrow("retained create failure");
    else await attempt;
    expect(order[2]).toBe("capture custody");
    expect(order[1]).toContain("create cluster");
    if (failed) expect(order).toHaveLength(3);
  });

  it("gives the migration journal sole ownership of protected publication", async () => {
    publishCore.mockResolvedValue(undefined);
    await installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart",
      ["/exact/values.yaml"], [], { receipt: ".migration.jsonl" });
    expect(execute).not.toHaveBeenCalled();
    expect(publishCore).toHaveBeenCalledWith(expect.any(Function), expect.objectContaining({
      context: "kind-chosen", receipt: ".migration.jsonl", release: "kars", namespace: "kars-system",
      chart: "/exact/chart", values: ["/exact/values.yaml"], set: [],
    }));
  });

  it("forwards explicit failed-publication recovery only to the protected journal owner", async () => {
    publishCore.mockResolvedValue(undefined);
    await installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart",
      ["/exact/values.yaml"], [], { receipt: ".migration.jsonl", recoverFailedPublication: true });
    expect(execute).not.toHaveBeenCalled();
    expect(publishCore).toHaveBeenCalledWith(expect.any(Function), expect.objectContaining({
      context: "kind-chosen", receipt: ".migration.jsonl", recoverFailedPublication: true,
    }));
  });

  it("never falls back to an unjournaled install when publication is ambiguous", async () => {
    publishCore.mockRejectedValue(new Error("Publication outcome is unknown"));
    await expect(installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart",
      [], [], { receipt: ".migration.jsonl" })).rejects.toThrow("Publication outcome is unknown");
    expect(execute).not.toHaveBeenCalled();
  });
  it.each([false, true])("pins rendering, schema preparation and final Helm install (existing Core=%s)", async existing => {
    const f = schemaFixture();
    f.options.releaseExists = existing;
    const globalConfig = { currentContext: "production-ambient" };
    const before = structuredClone(globalConfig);
    const finalInstall: string[][] = [];
    execute.mockImplementation(async (file, args, options) => {
      if (file === "kind") {
        expect(args).toEqual(["get", "clusters"]);
        return { stdout: "chosen\nother\n" };
      }
      expect(args).not.toContain("config");
      const contextFlag = file === "/tools/helm" ? "--kube-context" : "--context";
      const index = args.indexOf(contextFlag);
      expect(index).toBeGreaterThanOrEqual(0);
      expect(args[index + 1]).toBe("kind-chosen");
      const command = args.filter((_, position) => position !== index && position !== index + 1);
      if (file === "/tools/helm" && command[0] === "upgrade") {
        finalInstall.push([...args]);
        expect(args).toEqual(expect.arrayContaining(["--install", "kars", "/exact/chart", "--wait=legacy"]));
        expect(f.writes).toHaveLength(1);
      }
      return f.execute(file === "/tools/helm" ? "helm" : "kubectl", command, options ?? { stdio: "pipe" });
    });
    await ensureCluster("kind", "chosen", {}, "docker");
    await installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart", ["/exact/values.yaml"]);
    expect(finalInstall).toHaveLength(1);
    expect(f.writes).toHaveLength(1);
    expect(execute.mock.calls.some(([file, args]) => file === "kind" && args[0] === "create")).toBe(false);
    expect(globalConfig).toEqual(before);
    expect(f.requests.filter(request => request.args[0] === "template").map(request =>
      request.args.includes("--dry-run=server"))).toEqual(existing ? [false, true, true] : [false, true]);
    expect(f.requests.filter(request => request.args[0] === "template").map(request =>
      request.args.includes("--is-upgrade"))).toEqual(existing ? [true, true, true] : [false, false]);
  });

  it("passes the same captured original witness baseline to preflight and the final scoped upgrade", async () => {
    const f = schemaFixture();
    const witness = { managed: true, enabled: true, sandboxes: ["sre"], nodes: ["approved-node"],
      aggregator: { image: `registry.example/witness:latest@sha256:${"a".repeat(64)}` },
      gadget: { image: `registry.example/witness:latest@sha256:${"a".repeat(64)}`, hookMode: "podinformer" } };
    const baselineFlag = `sre.datapathWitnessBaseline=${JSON.stringify(witness)}`;
    const rendered: string[][] = [];
    const final: string[][] = [];
    execute.mockImplementation(async (file, args, options) => {
      expect(args.slice(0, 2)).toEqual([file === "/tools/helm" ? "--kube-context" : "--context", "kind-chosen"]);
      const command = args.slice(2);
      if (file === "/tools/kubectl" && command[2] === "kars-sre-helm-adoption") return { stdout: JSON.stringify({
        immutable: true, metadata: { name: "kars-sre-helm-adoption", namespace: "kars-system", uid: "adoption-uid", resourceVersion: "1",
          annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" } },
        data: { intentUID: "intent-uid", valuesDigest: "a".repeat(64) },
      }) };
      if (file === "/tools/helm" && command[0] === "get" && command[1] === "values") {
        expect(command).toContain("--all");
        return { stdout: JSON.stringify({ datapathWitness: witness }) };
      }
      if (command[0] === "template") rendered.push([...command]);
      if (command[0] === "upgrade") final.push([...command]);
      return f.execute(file === "/tools/helm" ? "helm" : "kubectl", command, options);
    });
    await installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart", [],
      ["datapathWitness.scope=all", "datapathWitness.sandboxes={}"]);
    expect(final).toHaveLength(1);
    expect(rendered.length).toBeGreaterThan(0);
    for (const args of [...rendered, ...final]) {
      expect(args).toContain(baselineFlag);
      expect(args).toContain("datapathWitness.scope=all");
      expect(args).not.toContain('datapathWitness.scope="selected"');
    }
  });

  it("refuses legacy template-owned schemas without adoption or a Helm installation", async () => {
    const f = schemaFixture();
    f.options.releaseExists = false;
    const { crd } = await import("../../lib/schema-stage.test-support.js");
    f.install(crd(), { ...f.owner, ownership: "template" });
    execute.mockImplementation((file, args, options) => {
      const command = args.filter((_, index) => index !== 0 && index !== 1);
      return f.execute(file === "/tools/helm" ? "helm" : "kubectl", command, options);
    });
    await expect(installLocalCoreChart("/tools/helm", "/tools/kubectl", "chosen", "kars", "/exact/chart", []))
      .rejects.toThrow("ownership");
    expect(f.writes).toEqual([]);
    expect(execute.mock.calls.some(([, args]) => args.includes("upgrade"))).toBe(false);
  });
});

describe("new Kind node journal retention", () => {
  const journal = "[Journal]\nRuntimeMaxUse=128M\nRuntimeMaxFileSize=16M\n";
  function fixture({ inventory = "chosen-control-plane\nchosen-worker", owner = "chosen",
    failWrite = false, echo = journal } = {}) {
    execute.mockImplementation(async (file, args, options) => {
      if (file === "kind") {
        if (args[0] === "create") return { stdout: "" };
        return { stdout: args[1] === "clusters" ? "" : inventory };
      }
      if (args[0] === "inspect") {
        return { stdout: `${(args.at(-1)?.endsWith("worker") ? "2" : "1").repeat(64)} ${owner}` };
      }
      expect(args.slice(0, 2)).toEqual(["exec", "-i"]);
      expect(args[2]).toMatch(/^[a-f0-9]{64}$/);
      expect(args.at(-1)).toContain("systemctl is-active --quiet systemd-journald");
      expect(args.at(-1)).not.toMatch(/vacuum|delete|memory\.max/);
      expect(options?.input).toBe(journal);
      if (failWrite) throw new Error("journal configuration failed");
      return { stdout: echo };
    });
  }

  it("adds cold Kind config/image/kubeconfig/retain arguments while preserving verified journal setup", async () => {
    fixture({ inventory: "chosen-control-plane" });
    await ensureCluster("kind", "chosen", {}, "docker", { config: "/reviewed/kind.yaml", kubeconfig: "/dedicated/config", image: "qualified-node" });
    expect(execute.mock.calls[1][1]).toEqual(["create", "cluster", "--name", "chosen", "--config", "/reviewed/kind.yaml", "--kubeconfig", "/dedicated/config", "--image", "qualified-node", "--retain"]);
    expect(execute.mock.calls.filter(([file, args]) => file === "docker" && args[0] === "exec")).toHaveLength(1);
  });

  it("verifies journal bytes without subprocess newline stripping", async () => {
    fixture({ inventory: "chosen-control-plane" });
    const simulated = execute.getMockImplementation()!;
    const { execa } = await vi.importActual<typeof import("execa")>("execa");
    execute.mockImplementation((file, args, options) => args[0] === "exec"
      ? execa(process.execPath, ["-e", "process.stdin.pipe(process.stdout)"], options)
      : simulated(file, args, options));
    await ensureCluster("kind", "chosen", {}, "docker");
    expect(execute.mock.calls.find(([, args]) => args[0] === "exec")?.[2])
      .toMatchObject({ input: journal, stripFinalNewline: false });
  });

  it.each(["docker", "podman", "nerdctl"])("bounds every fresh node through %s without changing runtime limits", async runtime => {
    fixture();
    await ensureCluster("kind", "chosen", { KIND_EXPERIMENTAL_PROVIDER: runtime }, runtime);
    const writes = execute.mock.calls.filter(([file, args]) => file === runtime && args[0] === "exec");
    expect(writes).toHaveLength(2);
    expect(writes.map(([, args]) => args[2])).toEqual(["1".repeat(64), "2".repeat(64)]);
    expect(execute.mock.calls[1][1]).toEqual(["create", "cluster", "--name", "chosen"]);
  });

  it.each(["", "chosen-worker\nchosen-worker", "other-worker", "chosen-worker;bad"])(
    "refuses invalid node inventory %j before any node mutation", async inventory => {
      fixture({ inventory });
      await expect(ensureCluster("kind", "chosen", {}, "docker")).rejects.toThrow("node inventory");
      expect(execute.mock.calls.some(([, args]) => args[0] === "exec")).toBe(false);
    });

  it("refuses a foreign cluster label before writing journal settings", async () => {
    fixture({ owner: "foreign" });
    await expect(ensureCluster("kind", "chosen", {}, "docker")).rejects.toThrow("unverified Kind node");
    expect(execute.mock.calls.some(([, args]) => args[0] === "exec")).toBe(false);
  });

  it("reports failed or unverified configuration instead of announcing a ready cluster", async () => {
    for (const options of [{ failWrite: true }, { echo: "wrong settings" }]) {
      fixture(options);
      await expect(ensureCluster("kind", "chosen", {}, "docker")).rejects.toThrow(/journal/);
    }
  });
});

describe("required Kind runtime images", () => {
  function imagesFixture(available: Set<string>, staleAfterImport = false, wrongArch = false, indexed = false,
    invalidIndex?: "schema" | "null" | "array" | "ambiguous" | "missing" | "digest" | "content",
    nodes = ["chosen-control-plane"], missingNodes: string[] = [], failedImportNodes: string[] = []) {
    const imported = new Set<string>();
    let savedImage = "";
    const digest = (bytes: string) => `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
    const manifest = JSON.stringify({ schemaVersion: 2, mediaType: "application/vnd.oci.image.manifest.v1+json",
      config: { digest: `sha256:${"1".repeat(64)}` } });
    const manifestDigest = digest(manifest);
    const descriptor = { digest: invalidIndex === "digest" ? "sha256:short" : manifestDigest,
      platform: { os: "linux", architecture: process.arch === "x64" ? "amd64" : process.arch } };
    const index = JSON.stringify(invalidIndex === "null" ? null : invalidIndex === "array" ? [] : {
      schemaVersion: invalidIndex === "schema" ? 1 : 2, mediaType: "application/vnd.oci.image.index.v1+json",
      manifests: invalidIndex === "ambiguous" ? [descriptor, descriptor] : invalidIndex === "missing" ? [] : [descriptor],
    });
    const indexDigest = digest(index);
    execute.mockImplementation(async (file, args) => {
      if (file === "kind") {
        if (args[0] === "get") {
          expect(args).toEqual(["get", "nodes", "--name", "chosen"]);
          return { stdout: nodes.join("\n") };
        }
        expect(args.slice(0, 2)).toEqual(["load", "docker-image"]);
        expect(args.slice(-2)).toEqual(["--name", "chosen"]);
        return { stdout: "" };
      }
      expect(file).toBe("docker");
      if (args[0] === "image") {
        if (!available.has(args[2])) throw new Error("No such image");
        if (args.includes("{{.Architecture}}")) {
          return { stdout: wrongArch ? "unsupported-architecture" : process.arch === "x64" ? "amd64" : process.arch };
        }
        return { stdout: indexed ? indexDigest : `sha256:${"1".repeat(64)}` };
      }
      if (args[0] === "tag") {
        expect(available.has(args[1])).toBe(true);
        available.add(args[2]);
        return { stdout: "" };
      }
      if (args[0] === "exec" && args[2] === "crictl") {
        expect(nodes).toContain(args[1]);
        if (missingNodes.includes(args[1])
          && (!imported.has(`${args[1]}/${args.at(-1)}`) || failedImportNodes.includes(args[1]))) {
          throw new Error(`Image absent on ${args[1]}`);
        }
        return { stdout: JSON.stringify({ status: { id: `sha256:${(staleAfterImport ? "2" : "1").repeat(64)}` } }) };
      }
      if (args[0] === "exec" && args[1] === "-i" && args.includes("import")) {
        expect(nodes).toContain(args[2]);
        imported.add(`${args[2]}/${savedImage}`);
        return { stdout: "" };
      }
      if (args[0] === "save") {
        savedImage = args[1];
        return { stdout: "" };
      }
      if (indexed && args.includes("content")) {
        if (args.at(-1) === indexDigest) return { stdout: invalidIndex === "content" ? index + " " : index };
        if (args.at(-1) === manifestDigest) return { stdout: manifest };
        throw new Error("Unexpected content identity");
      }
      if (staleAfterImport && args.includes("content")) throw new Error("Source descriptor unavailable");
      if ((staleAfterImport || invalidIndex) && (args[0] === "save" || args.includes("ctr"))) return { stdout: "" };
      throw new Error(`Unexpected command: ${file} ${args.join(" ")}`);
    });
  }

  it("includes both default harnesses in the same release and local load plan", () => {
    const images = localK8sImagePlan("custom-openclaw:latest");
    expect(images.map(image => image.repo)).toEqual([
      "kars-prometheus", "kars-prometheus-operator", "kars-prometheus-config-reloader",
      "kars-node-exporter", "kars-webhook-certgen", "kars-kube-state-metrics", "kars-json-exporter",
      "kars-metrics-server", "kars-headlamp", "openclaw-sandbox", "kars-controller",
      "kars-inference-router", "kars-runtime-hermes", "kars-conformance-runner",
    ]);
    expect(images.find(image => image.repo === "openclaw-sandbox")?.aliases).toContain("custom-openclaw:latest");
    expect(images.find(image => image.repo === "kars-runtime-hermes")).toEqual({
      repo: "kars-runtime-hermes", target: "kars-runtime-hermes:latest",
      aliases: ["karsacr.azurecr.io/kars-runtime-hermes:latest", "kars-runtime-hermes:dev"],
    });
  });

  it("loads both harnesses and the safety runner alongside Core images into the explicit cluster", async () => {
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)));
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev")).resolves.toBe(14);
    expect(execute.mock.calls.filter(([file, args]) => file === "kind" && args[0] === "load")).toHaveLength(14);
  });

  it("retags an existing private-named Hermes image to the configured local name", async () => {
    const available = new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target));
    available.delete("kars-runtime-hermes:latest");
    available.add("karsacr.azurecr.io/kars-runtime-hermes:latest");
    imagesFixture(available);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev")).resolves.toBe(14);
    expect(execute.mock.calls.some(([file, args]) => file === "docker" &&
      args.join(" ") === "tag karsacr.azurecr.io/kars-runtime-hermes:latest kars-runtime-hermes:latest")).toBe(true);
  });

  it("verifies containerd-backed Docker indexes against the exact native CRI config without redundant imports", async () => {
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)), false, false, true);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev")).resolves.toBe(14);
    expect(execute.mock.calls.filter(([, args]) => args.includes("content"))).toHaveLength(28);
    expect(execute.mock.calls.some(([, args]) => args.includes("save") || args.includes("import"))).toBe(false);
  });

  it("verifies the native image identity on every Kind worker without unnecessary imports", async () => {
    const nodes = ["chosen-control-plane", "chosen-worker", "chosen-worker2"];
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)),
      false, false, true, undefined, nodes);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev")).resolves.toBe(14);
    for (const node of nodes) {
      expect(execute.mock.calls.filter(([, args]) => args[1] === node && args[2] === "crictl")).toHaveLength(14);
    }
    expect(execute.mock.calls.filter(([, args]) => args.includes("content"))).toHaveLength(84);
    expect(execute.mock.calls.some(([, args]) => args[0] === "save" || args.includes("import"))).toBe(false);
  });

  it("repairs a missing worker image without reimporting into a verified control plane", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)),
      false, false, false, undefined, ["chosen-control-plane", "chosen-worker"], ["chosen-worker"]);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev")).resolves.toBe(14);
    const imports = execute.mock.calls.filter(([, args]) => args.includes("import"));
    expect(imports).toHaveLength(14);
    for (const [, args] of imports) {
      expect(args).toEqual(["exec", "-i", "chosen-worker", "ctr", "-n=k8s.io", "images", "import", "-"]);
    }
  });

  it("refuses success when the control plane is correct but a worker remains unqualified", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)),
      false, false, false, undefined, ["chosen-control-plane", "chosen-worker"],
      ["chosen-worker"], ["chosen-worker"]);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev"))
      .rejects.toThrow("Image absent on chosen-worker");
    expect(execute.mock.calls.filter(([, args]) => args.includes("import"))).toHaveLength(1);
  });

  it.each([
    { nodes: [] },
    { nodes: ["chosen-control-plane", "chosen-control-plane"] },
    { nodes: ["production-worker"] },
    { nodes: ["chosen-worker extra"] },
  ])(
    "refuses invalid node inventory $nodes before loading images", async ({ nodes }) => {
      imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)),
        false, false, false, undefined, nodes);
      await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev"))
        .rejects.toThrow("Invalid or empty Kind node inventory");
      expect(execute.mock.calls.some(([file, args]) => file === "kind" && args[0] === "load")).toBe(false);
      expect(execute.mock.calls.some(([, args]) => args[0] === "save" || args[0] === "exec")).toBe(false);
    },
  );

  it.each([
    ["schema", "Unsupported image descriptor"],
    ["null", "Unsupported image descriptor"],
    ["array", "Unsupported image descriptor"],
    ["ambiguous", "Ambiguous or missing native image platform"],
    ["missing", "Ambiguous or missing native image platform"],
    ["digest", "Invalid image descriptor"],
    ["content", "Image descriptor content mismatch"],
  ] as const)("rejects an invalid %s index even after direct import", async (invalidIndex, message) => {
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)),
      false, false, true, invalidIndex);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev"))
      .rejects.toThrow(message);
    expect(warning).toHaveBeenCalledWith(expect.stringContaining(message));
    expect(execute.mock.calls.filter(([, args]) => args[0] === "save")).toHaveLength(1);
  });

  it.each(["kars-runtime-hermes:latest", "kars-inference-router:dev", "kars-conformance-runner:latest", "kars-headlamp:latest", "kars-prometheus:latest", "kars-webhook-certgen:latest"])("rejects missing required image %s", async missing => {
    const available = new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target));
    available.delete(missing);
    imagesFixture(available);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev"))
      .rejects.toThrow(`Required local image unavailable: '${missing}'`);
    expect(execute.mock.calls.every(([file]) => file === "docker" || file === "kind")).toBe(true);
  });

  it("does not count an import that leaves a stale image as loaded", async () => {
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)), true);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev"))
      .rejects.toThrow("Source descriptor unavailable");
  });

  it("requires an explicit target before any image operations", async () => {
    await expect(loadRequiredLocalImages("kind", "docker", "", {}, "kars-sandbox:dev"))
      .rejects.toThrow("cluster must be explicit");
    expect(execute).not.toHaveBeenCalled();
  });

  it("rejects non-native release images before loading them into Kind", async () => {
    imagesFixture(new Set(localK8sImagePlan("kars-sandbox:dev").map(image => image.target)), false, true);
    await expect(loadRequiredLocalImages("kind", "docker", "chosen", {}, "kars-sandbox:dev"))
      .rejects.toThrow("has architecture unsupported-architecture");
    expect(execute.mock.calls.some(([file]) => file === "kind")).toBe(false);
  });

  it("builds the actual local eval image using Linux compilation, never a host macOS binary", async () => {
    execute.mockResolvedValue({ stdout: "" });
    const arch = process.arch === "arm64" ? "arm64" : "amd64";
    await buildLocalConformanceRunnerImage("docker", "/repo", arch);
    const multistage = process.platform !== "linux";
    expect(stageRust.mock.calls).toEqual(multistage ? [] : [["/repo", ["kars-conformance-runner"], arch]]);
    expect(execute).toHaveBeenCalledWith("docker", [
      "build", "--platform", `linux/${arch}`, "-t", "kars-conformance-runner:latest",
      "-f", `/repo/sandbox-images/conformance-runner/Dockerfile${multistage ? ".multistage" : ""}`, "/repo",
    ], { stdio: "inherit" });
  });
});
