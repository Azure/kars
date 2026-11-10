// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const cluster = "bridge-native";
const context = "kind-bridge-native";
const expectedNodes = ["bridge-native-control-plane", "bridge-native-worker"];
const publicCertificate = "/var/lib/kubelet/pki/kubelet.crt";

function require(condition) {
  if (!condition) throw new Error("Native metrics requires stable owned Kind nodes and verified kubelet trust");
}

function execute(args) {
  const result = spawnSync(args[0], args.slice(1), {
    encoding: "utf8", timeout: 15_000, maxBuffer: 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
  require(!result.error && result.status === 0);
  return result.stdout;
}

function nodeIdentity(node) {
  return { name: node.metadata?.name, uid: node.metadata?.uid, addresses: node.status?.addresses };
}

function containerIdentity(container) {
  require(typeof container.Id === "string" && container.Id.length > 0);
  return { id: container.Id, name: container.Name, running: container.State?.Running,
    labels: container.Config?.Labels, networks: container.NetworkSettings?.Networks };
}

export function nativeMetricsTrust(run, verifyNodeTrust) {
  const inventory = () => {
    const owned = run(["kind", "get", "nodes", "--name", cluster]).trim().split(/\s+/).sort();
    require(JSON.stringify(owned) === JSON.stringify(expectedNodes));
    const nodes = JSON.parse(run(["kubectl", "--context", context, "get", "nodes", "-o", "json"])).items;
    require(Array.isArray(nodes) && nodes.length === expectedNodes.length);
    nodes.sort((a, b) => String(a.metadata?.name).localeCompare(String(b.metadata?.name)));
    require(nodes.every((node, index) => node.metadata?.name === expectedNodes[index] && node.metadata?.uid));
    return nodes;
  };
  const inspect = name => {
    const containers = JSON.parse(run(["docker", "inspect", name]));
    require(Array.isArray(containers) && containers.length === 1);
    return containers[0];
  };
  const nodes = inventory();
  const containers = nodes.map(node => inspect(node.metadata.name));
  const identities = containers.map(containerIdentity);
  const trusts = nodes.map((node, index) => verifyNodeTrust(cluster, node, containers[index],
    run(["docker", "exec", node.metadata.name, "cat", publicCertificate])));
  const latest = inventory();
  require(JSON.stringify(nodes.map(nodeIdentity)) === JSON.stringify(latest.map(nodeIdentity)));
  require(JSON.stringify(identities) === JSON.stringify(latest.map(node => containerIdentity(inspect(node.metadata.name)))));
  return {
    hostAliases: trusts.flatMap(trust => trust.hostAliases),
    kubelet: { insecureTLS: false, preferredAddressTypes: ["Hostname"],
      caBundle: [...new Set(trusts.map(trust => trust.kubelet.caBundle.trim()))].join("\n") + "\n" },
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const { kindNodeKubeletTrust } = await import("../../.native/core/cli/dist/lib/kind-kubelet-trust.js");
    process.stdout.write(JSON.stringify(nativeMetricsTrust(execute, kindNodeKubeletTrust)));
  } catch {
    process.stderr.write("Native metrics kubelet trust verification failed\n");
    process.exitCode = 1;
  }
}
