// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execa } from "execa";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parse, stringify } from "yaml";
import { requireBundledAsset } from "../../lib/repo-assets.js";
import { isMonitoringCrd, planMonitoringSchemas } from "../../lib/monitoring-schemas.js";
import { schemaDocuments, type SchemaExecute } from "../../lib/schema-documents.js";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export async function installMonitoring(
  context: string,
  tools = { kubectl: "kubectl", helm: "helm" },
): Promise<void> {
  const kubectl = (args: string[], input?: string) =>
    execa(tools.kubectl, ["--context", context, ...args], { stdio: "pipe", input });
  const { stdout: namespaceJSON } = await kubectl([
    "get", "namespace", "monitoring", "--ignore-not-found", "-o", "json",
  ]);
  if (namespaceJSON.trim()) {
    const namespace: unknown = JSON.parse(namespaceJSON);
    if (!isRecord(namespace) || namespace.kind !== "Namespace" || !isRecord(namespace.metadata)
      || namespace.metadata.name !== "monitoring") throw new Error("Invalid monitoring namespace response");
    const annotations = namespace.metadata.annotations;
    if (annotations !== undefined && !isRecord(annotations)) throw new Error("Invalid monitoring ownership annotations");
    if (isRecord(annotations)) {
      const owner = annotations["kars.azure.com/monitoring-owner"];
      const release = annotations["meta.helm.sh/release-name"];
      const releaseNamespace = annotations["meta.helm.sh/release-namespace"];
      if (owner !== undefined || release !== undefined || releaseNamespace !== undefined) {
        if (typeof owner !== "string" || typeof release !== "string" || typeof releaseNamespace !== "string"
          || !/^[^/\s]+\/[^/\s]+$/.test(owner)
          || owner !== `${releaseNamespace}/${release}`) {
          throw new Error("Monitoring namespace has external or conflicting ownership; refusing a standalone installation");
        }
        console.log("  Monitoring is Core-managed; update its images and configuration through the Core release.");
        return;
      }
    }
    const labels = namespace.metadata.labels;
    if (!isRecord(labels) || labels["app.kubernetes.io/name"] !== "kars"
      || labels["app.kubernetes.io/component"] !== "system") {
      throw new Error("Existing monitoring namespace has no proven Kars ownership; refusing to relabel it");
    }
  }
  const { stdout: controllerJSON } = await kubectl([
    "get", "deployment", "kars-controller", "-n", "kars-system", "--ignore-not-found", "-o", "json",
  ]);
  if (controllerJSON.trim()) {
    const controller: unknown = JSON.parse(controllerJSON);
    if (!isRecord(controller) || controller.kind !== "Deployment" || !isRecord(controller.metadata)
      || controller.metadata.name !== "kars-controller") throw new Error("Invalid Core controller response");
    throw new Error("Core is installed without Core-owned monitoring; enable monitoring.enabled through a reviewed Core upgrade, or keep your external monitoring. No standalone resources were changed.");
  }

  const chart = requireBundledAsset("deploy/helm/kars");
  const defaults: unknown = parse(readFileSync(join(chart, "values.yaml"), "utf8"));
  const metadata: unknown = parse(readFileSync(join(chart, "Chart.yaml"), "utf8"));
  if (!isRecord(defaults) || !isRecord(defaults.monitoring)
    || defaults.monitoring.enabled !== true || !isRecord(defaults.monitoring.prometheus)
    || defaults.monitoring.prometheus.enabled !== true || !isRecord(defaults.monitoring.grafana)
    || !isRecord(defaults.monitoring.jsonExporter)
    || !isRecord(metadata) || !Array.isArray(metadata.dependencies)) {
    throw new Error("Bundled Core monitoring configuration is invalid");
  }
  const monitoring = defaults.monitoring;
  const grafanaEnabled = defaults.monitoring.grafana.enabled === true;
  const jsonExporterEnabled = defaults.monitoring.jsonExporter.enabled === true;
  const dependency: unknown = metadata.dependencies.find((item: unknown) =>
    isRecord(item) && item.name === "kube-prometheus-stack" && item.alias === "monitoring");
  if (!isRecord(dependency) || typeof dependency.version !== "string"
    || !/^\d+\.\d+\.\d+$/.test(dependency.version)) throw new Error("Pinned Core monitoring dependency is missing");
  const archive = join(chart, "charts", `kube-prometheus-stack-${dependency.version}.tgz`);
  if (!existsSync(archive)) throw new Error("Core monitoring dependency is not bundled; build the chart dependencies before installing");

  const temporary = mkdtempSync(join(tmpdir(), "kars-monitoring-"));
  try {
    const values = join(temporary, "values.yaml");
    writeFileSync(values, stringify(monitoring));
    const { stdout: upstream } = await execa(tools.helm, [
      "template", "kps", archive, "-n", "monitoring", "--include-crds", "-f", values,
    ], { stdio: "pipe" });
    const templates = ["monitoring-podmonitor.yaml", "monitoring-query-rbac.yaml"];
    if (jsonExporterEnabled) templates.push("monitoring-json-exporter.yaml");
    if (grafanaEnabled) templates.push("monitoring-dashboards.yaml");
    const { stdout: addons } = await execa(tools.helm, [
      "template", "kars", chart, "-n", "kars-system",
      ...templates.flatMap(template => ["--show-only", `templates/${template}`]),
    ], { stdio: "pipe" });
    const documents = schemaDocuments(upstream);
    schemaDocuments(addons);
    if (documents.some(object => object.kind === "CustomResourceDefinition" && !isMonitoringCrd(object))) {
      throw new Error("Monitoring chart contains an unsupported dependency CRD");
    }
    const run: SchemaExecute = (_file, args, options) => execa(tools.kubectl,
      ["--context", context, ...args], options);
    const applySchemas = await planMonitoringSchemas(run, documents);
    const apply = (manifest: string) => kubectl([
      "apply", "--server-side", "--field-manager=kars-monitoring", "-f", "-",
    ], manifest);
    await applySchemas();
    if (!namespaceJSON.trim()) {
      await kubectl(["create", "-f", "-"], stringify({ apiVersion: "v1", kind: "Namespace", metadata: {
        name: "monitoring", labels: { "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "system" },
      } }));
    }
    // Helm must order certificate creation and webhook patch hooks, not SSA.
    await execa(tools.helm, ["upgrade", "--install", "kps", archive, "-n", "monitoring",
      "--kube-context", context, "-f", values, "--skip-crds", "--wait", "--timeout", "5m"], { stdio: "pipe" });
    await apply(addons);
    await kubectl(["rollout", "status", "deployment/kps-kube-prometheus-stack-operator", "-n", "monitoring", "--timeout=180s"]);
    const prometheus = "statefulset/prometheus-kps-kube-prometheus-stack-prometheus";
    await kubectl(["wait", "--for=create", prometheus, "-n", "monitoring", "--timeout=120s"]);
    await kubectl(["rollout", "status", prometheus, "-n", "monitoring", "--timeout=180s"]);
    if (grafanaEnabled) {
      await kubectl(["rollout", "status", "deployment/kps-grafana", "-n", "monitoring", "--timeout=180s"]);
    }
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}
