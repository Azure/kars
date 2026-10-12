// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { parse, parseAllDocuments } from "yaml";
import { beforeAll, describe, expect, it } from "vitest";

const root = fileURLToPath(new URL("../../../", import.meta.url));
type Dashboard = {
  templating: { list: { name: string; query: unknown }[] };
  panels: { title: string; description?: string; targets?: { expr: string }[] }[];
};
type DatasourceConfig = {
  prune?: boolean;
  datasources: { type: string; url: string }[];
};
async function render(args: string[] = []) {
  const { stdout } = await execa("helm", ["template", "kars", `${root}deploy/helm/kars`,
    "-n", "kars-system", ...args], { stdio: "pipe" });
  return parseAllDocuments(stdout).map(document => {
    if (document.errors.length) throw document.errors[0];
    return document.toJSON();
  }).filter(Boolean);
}
let objects: Awaited<ReturnType<typeof render>>;
beforeAll(async () => { objects = await render(); });

describe("Core monitoring dependency", () => {
  it("preserves the proxy endpoint and uses the exact AL3 images, including admission hooks and reloader", () => {
    const podImages = objects.filter(o => o.metadata.namespace === "monitoring")
      .flatMap(o => o.spec?.template?.spec?.containers?.map((c: { image: string }) => c.image) ?? []);
    expect(new Set(podImages)).toEqual(new Set([
      "ghcr.io/azure/kars-node-exporter:latest", "ghcr.io/azure/kars-kube-state-metrics:latest",
      "ghcr.io/azure/kars-prometheus-operator:latest", "ghcr.io/azure/kars-json-exporter:latest",
      "ghcr.io/azure/kars-webhook-certgen:latest",
    ]));
    const prometheus = objects.find(o => o.kind === "Prometheus");
    expect(prometheus.metadata).toMatchObject({ name: "kps-kube-prometheus-stack-prometheus", namespace: "monitoring" });
    expect(prometheus.spec).toMatchObject({
      image: "ghcr.io/azure/kars-prometheus:latest", version: "v3.11.3",
      imagePullPolicy: "Always", enableAdminAPI: false,
      retention: "2d", retentionSize: "512MB", storage: { emptyDir: { sizeLimit: "1Gi" } },
      podMonitorSelector: {}, serviceMonitorSelector: {},
    });
    const operator = objects.find(o => o.kind === "Deployment" && o.metadata.name.endsWith("-operator"));
    expect(operator.spec.template.spec.containers[0].args)
      .toContain("--prometheus-config-reloader=ghcr.io/azure/kars-prometheus-config-reloader:latest");
    expect(objects.find(o => o.kind === "Service" && o.metadata.name === prometheus.metadata.name).spec.ports)
      .toEqual(expect.arrayContaining([expect.objectContaining({ port: 9090 })]));
  });

  it("bootstraps the namespace before admission hooks without broad user or dashboard grants", () => {
    const namespace = objects.find(o => o.kind === "Namespace" && o.metadata.name === "monitoring");
    expect(namespace.metadata).toMatchObject({
      labels: { "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "system" },
      annotations: { "kars.azure.com/monitoring-owner": "kars-system/kars",
        "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system",
        "helm.sh/hook": "pre-install,pre-upgrade", "helm.sh/hook-weight": "-40", "helm.sh/resource-policy": "keep" },
    });
    expect(objects.find(o => o.kind === "Role" && o.metadata.name === "kars-prometheus-query").rules)
      .toEqual([{ apiGroups: [""], resources: ["services/proxy"],
        resourceNames: ["kps-kube-prometheus-stack-prometheus:9090"], verbs: ["get"] }]);
    expect(objects.some(o => o.kind === "RoleBinding" && o.metadata.name === "kars-prometheus-query")).toBe(false);
  });

  it("grants only explicitly mapped users and groups the exact query role", async () => {
    const subjects = [{ kind: "User", name: "kars:operator@example.test" }, { kind: "Group", name: "kars:auditors" }];
    const result = await render(["--set-json", `monitoring.querySubjects=${JSON.stringify(subjects)}`]);
    expect(result.find(o => o.kind === "RoleBinding" && o.metadata.name === "kars-prometheus-query"))
      .toMatchObject({ metadata: { namespace: "monitoring" },
        roleRef: { kind: "Role", name: "kars-prometheus-query" },
        subjects: subjects.map(subject => ({ ...subject, apiGroup: "rbac.authorization.k8s.io" })) });
    await expect(render(["--set-json", 'monitoring.querySubjects=[{"kind":"ServiceAccount","name":"headlamp"}]']))
      .rejects.toThrow("monitoring.querySubjects");
  });

  it("keeps sandbox labels and real relay fields without granting the exporter a token or host root", () => {
    const monitor = objects.find(o => o.kind === "PodMonitor" && o.metadata.name === "kars-sandbox-router");
    expect(monitor.spec).toMatchObject({
      namespaceSelector: { any: true }, selector: { matchLabels: { "kars.azure.com/component": "sandbox" } },
      podMetricsEndpoints: [{ port: "inference", path: "/metrics", interval: "15s",
        scrapeTimeout: "10s", scheme: "http", relabelings: [
          { sourceLabels: ["__meta_kubernetes_pod_label_kars_azure_com_sandbox"], targetLabel: "sandbox" },
          { sourceLabels: ["__meta_kubernetes_namespace"], targetLabel: "sandbox_namespace" },
        ] }],
    });
    const exporter = objects.find(o => o.kind === "Deployment" && o.metadata.name === "agentmesh-json-exporter").spec.template.spec;
    expect(exporter.automountServiceAccountToken).toBe(false);
    expect(exporter.containers[0].securityContext).toMatchObject({ readOnlyRootFilesystem: true, allowPrivilegeEscalation: false });
    expect(objects.find(o => o.kind === "ServiceMonitor" && o.metadata.name === "agentmesh-relay-health").spec.endpoints[0].params)
      .toEqual({ target: ["http://agentmesh-relay.agentmesh.svc:8765/health"] });
    const node = objects.find(o => o.kind === "DaemonSet" && o.metadata.name === "kps-prometheus-node-exporter").spec.template.spec;
    expect(node.hostNetwork).not.toBe(true);
    expect(node.hostPID).not.toBe(true);
    expect(node.volumes.filter((v: { hostPath?: unknown }) => v.hostPath).map((v: { hostPath: { path: string } }) => v.hostPath.path))
      .toEqual(["/proc", "/sys"]);
  });

  it("honors whole-stack, Prometheus and exporter opt-outs", async () => {
    expect((await render(["--set", "monitoring.enabled=false"])).some(o =>
      o.metadata.namespace === "monitoring" || o.kind === "Namespace" && o.metadata.name === "monitoring")).toBe(false);
    const noPrometheus = await render(["--set", "monitoring.prometheus.enabled=false"]);
    expect(noPrometheus.some(o => ["Prometheus", "PodMonitor"].includes(o.kind))).toBe(false);
    expect(noPrometheus.some(o => o.metadata.name === "agentmesh-json-exporter")).toBe(false);
    expect((await render(["--set", "monitoring.jsonExporter.enabled=false"])).some(o =>
      o.metadata.name.startsWith("agentmesh-json-exporter") || o.metadata.name === "agentmesh-relay-health")).toBe(false);
  });

  it("renders optional dashboards from their maintained JSON and only AL3 Grafana containers", async () => {
    const result = await render(["--set", "monitoring.grafana.enabled=true"]);
    const pod = result.find(o => o.kind === "Deployment" && o.metadata.name === "kps-grafana").spec.template.spec;
    const containers = [...pod.containers, ...(pod.initContainers ?? [])];
    expect(result.find(o => o.kind === "ConfigMap" && o.metadata.name === "kps-grafana").data["grafana.ini"])
      .toContain("[database]\nwal = true");
    const grafana = containers.find(c => c.name === "grafana");
    expect(grafana.resources.limits).toEqual({ cpu: "500m", memory: "512Mi" });
    expect(grafana.env.filter((e: { name: string }) => e.name === "GOGC"))
      .toEqual([{ name: "GOGC", value: "50" }]);
    expect(grafana.env.filter((e: { name: string }) => e.name === "GOMEMLIMIT"))
      .toEqual([{ name: "GOMEMLIMIT", valueFrom: {
        resourceFieldRef: { divisor: "1", resource: "limits.memory" },
      } }]);
    expect(containers.every(c => ["ghcr.io/azure/kars-grafana:latest", "ghcr.io/azure/kars-k8s-sidecar:latest"].includes(c.image))).toBe(true);
    expect(containers.filter(c => c.image.includes("sidecar")).every(c =>
      c.env.some((e: { name: string; value: string }) => e.name === "RESOURCE" && e.value === "configmap"))).toBe(true);
    for (const name of ["fleet", "ops"]) {
      const dashboard = result.find(o => o.kind === "ConfigMap" && o.metadata.name === `kars-${name}-dashboard`);
      expect(JSON.parse(dashboard.data[`kars-${name}.json`]))
        .toEqual(JSON.parse(readFileSync(`${root}deploy/helm/kars/files/monitoring/grafana-dashboard-kars-${name}.json`, "utf8")));
    }
    expect(result.some(o => o.kind === "Pod" && o.metadata.annotations?.["helm.sh/hook"] === "test")).toBe(false);
  });

  it("discovers idle sandboxes from scrapes and distinguishes counters from durable evidence", async () => {
    const result = await render(["--set", "monitoring.grafana.enabled=true"]);
    for (const name of ["fleet", "ops"]) {
      const cm = result.find(o => o.kind === "ConfigMap" && o.metadata.name === `kars-${name}-dashboard`);
      const dashboard: Dashboard = JSON.parse(cm.data[`kars-${name}.json`]);
      expect(dashboard.templating.list.find(v => v.name === "sandbox")?.query)
        .toMatchObject({ query: 'label_values(up{job="monitoring/kars-sandbox-router"}, sandbox)' });
      const count = dashboard.panels.find(p => p.title === "Scraped sandbox routers");
      expect(count?.description).toContain("not workload readiness");
      expect(count?.targets?.[0].expr)
        .toBe('count(count by (sandbox) (up{job="monitoring/kars-sandbox-router",sandbox=~"$sandbox"} == 1))');
      if (name === "fleet") {
        expect(dashboard.panels.find(p => p.title === "Token counter sum (current routers)")?.description)
          .toContain("Not lifetime budget accounting");
      } else {
        const error = dashboard.panels.find(p => p.title === "Error rate (5m)");
        expect(error?.targets?.[0].expr).toBe(
          '(sum(rate(kars_inference_requests_total{status!="ok",sandbox=~"$sandbox"}[5m]))'
          + ' or (0 * sum(rate(kars_inference_requests_total{sandbox=~"$sandbox"}[5m]))))'
          + ' / (sum(rate(kars_inference_requests_total{sandbox=~"$sandbox"}[5m])) > 0) * 100');
        expect(dashboard.panels.find(p => p.title === "Retained audit entries (router sum)")?.targets?.[0].expr)
          .toBe('sum(kars_agt_audit_entries_total{sandbox=~"$sandbox"})');
        expect(dashboard.panels.find(p => p.title === "Behavior alerts (cumulative)")?.description)
          .toContain("not currently active incidents");
        expect(dashboard.panels.find(p => p.title === "Trust-store peers (router sum)")?.description)
          .toContain("counted more than once");
        expect(dashboard.panels.find(p => p.title === "Estimated cost rate vs display threshold")?.description)
          .toContain("not an enforced budget");
      }
    }
  });

  it("omits the absent Alertmanager datasource and prunes removed provisioned entries", async () => {
    const result = await render(["--set", "monitoring.grafana.enabled=true"]);
    const cm = result.find(o => o.kind === "ConfigMap" && o.metadata.name.endsWith("-grafana-datasource"));
    const provisioning: DatasourceConfig = parse(cm.data["datasource.yaml"]);
    expect(provisioning.prune).toBe(true);
    expect(provisioning.datasources.map(d => d.type)).toEqual(["prometheus"]);
    const optedIn = await render(["--set", "monitoring.grafana.enabled=true",
      "--set", "monitoring.grafana.sidecar.datasources.alertmanager.enabled=true",
      "--set-string", "monitoring.grafana.sidecar.datasources.alertmanager.url=https://alerts.example.test"]);
    const external: DatasourceConfig = parse(optedIn.find(o => o.kind === "ConfigMap"
      && o.metadata.name.endsWith("-grafana-datasource")).data["datasource.yaml"]);
    expect(external.datasources.find(d => d.type === "alertmanager")?.url).toBe("https://alerts.example.test");
  });

  it("rejects endpoint namespace drift", async () => {
    for (const setting of ["namespaceOverride", "grafana.namespaceOverride", "kube-state-metrics.namespaceOverride", "prometheus-node-exporter.namespaceOverride"]) {
      await expect(render(["--set-string", `monitoring.${setting}=elsewhere`])).rejects.toThrow("monitoring");
    }
  });
});
