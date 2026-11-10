// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { nativeMetricsTrust } from "./metrics-trust.mjs";

const names = ["bridge-native-control-plane", "bridge-native-worker"];
function fixture({ mutate = () => {}, verify = null, ca = () => "public-ca" } = {}) {
  const calls = [], verified = [];
  let inventoryReads = 0;
  const run = args => {
    calls.push(args);
    let value;
    if (args[0] === "kind") return names.join("\n") + "\n";
    if (args[0] === "kubectl") {
      assert.deepEqual(args, ["kubectl", "--context", "kind-bridge-native", "get", "nodes", "-o", "json"]);
      inventoryReads++;
      value = { items: names.map((name, index) => ({ metadata: { name, uid: `node-${index}` },
        status: { addresses: [{ type: "InternalIP", address: `172.18.0.${index + 2}` },
          { type: "Hostname", address: name }] } })) };
    } else if (args[1] === "inspect") {
      value = [{ Id: `container-${args[2]}`, Name: `/${args[2]}`, State: { Running: true },
        Config: { Labels: { "io.x-k8s.kind.cluster": "bridge-native" } } }];
    } else {
      assert.deepEqual(args.slice(0, 2), ["docker", "exec"]);
      assert.ok(names.includes(args[2]));
      assert.deepEqual(args.slice(3), ["cat", "/var/lib/kubelet/pki/kubelet.crt"]);
      return `public-chain-${args[2]}`;
    }
    mutate(value, args, inventoryReads);
    return JSON.stringify(value);
  };
  const validate = (cluster, node, container, pem) => {
    verified.push([cluster, node, container, pem]);
    if (verify) verify(cluster, node, container, pem);
    return { hostAliases: [{ ip: node.status.addresses[0].address, hostnames: [node.metadata.name] }],
      kubelet: { insecureTLS: false, preferredAddressTypes: ["Hostname"], caBundle: ca(node) } };
  };
  return { run, validate, calls, verified };
}

test("collects both exact owned nodes and delegates their public chains to maintained validation", () => {
  const { run, validate, calls, verified } = fixture();
  assert.deepEqual(nativeMetricsTrust(run, validate), {
    hostAliases: names.map((name, index) => ({ ip: `172.18.0.${index + 2}`, hostnames: [name] })),
    kubelet: { insecureTLS: false, preferredAddressTypes: ["Hostname"], caBundle: "public-ca\n" },
  });
  assert.equal(verified.length, 2);
  for (const [index, [cluster, node, container, pem]] of verified.entries()) {
    assert.equal(cluster, "bridge-native");
    assert.equal(node.metadata.name, names[index]);
    assert.equal(container.Name, `/${names[index]}`);
    assert.equal(pem, `public-chain-${names[index]}`);
  }
  assert.equal(calls.filter(args => args[0] === "kind").length, 2);
  assert.equal(calls.filter(args => args[1] === "inspect").length, 4);
  assert.ok(calls.every(args => !args.includes("secrets") && !args.includes("configmaps")));
});

test("retains each node CA when kubelets have distinct signing roots", () => {
  const { run, validate } = fixture({ ca: node => `ca-${node.metadata.name}` });
  assert.equal(nativeMetricsTrust(run, validate).kubelet.caBundle,
    names.map(name => `ca-${name}`).join("\n") + "\n");
});

test("fails closed when Kind lists an unexpected or missing node", () => {
  for (const inventory of [names[0], [...names, "foreign-worker"].join("\n")]) {
    const { run, validate, verified } = fixture();
    assert.throws(() => nativeMetricsTrust(args => args[0] === "kind" ? inventory : run(args), validate));
    assert.equal(verified.length, 0);
  }
});

test("fails closed for duplicate Kubernetes nodes even with correct Kind inventory", () => {
  const { run, validate, verified } = fixture({ mutate(value, args) {
    if (args[0] === "kubectl") value.items[1] = structuredClone(value.items[0]);
  } });
  assert.throws(() => nativeMetricsTrust(run, validate));
  assert.equal(verified.length, 0);
});

for (const field of ["uid", "addresses", "container"]) {
  test(`fails closed when ${field} changes after public certificate verification`, () => {
    const { run, validate } = fixture({ mutate(value, args, reads) {
      if (reads !== 2) return;
      if (args[0] === "kubectl" && field === "uid") value.items[0].metadata.uid = "replaced";
      if (args[0] === "kubectl" && field === "addresses") value.items[0].status.addresses[0].address = "172.18.0.9";
      if (args[1] === "inspect" && field === "container") value[0].Id = "replaced";
    } });
    assert.throws(() => nativeMetricsTrust(run, validate));
  });
}

test("propagates certificate validation rejection without returning Helm values", () => {
  const { run, validate } = fixture({ verify() { throw new Error("certificate rejected"); } });
  assert.throws(() => nativeMetricsTrust(run, validate), /certificate rejected/);
});

test("production entry uses the compiled shared verifier and only a fixed failure diagnostic", () => {
  const source = readFileSync(new URL("./metrics-trust.mjs", import.meta.url), "utf8");
  assert.ok(source.includes('await import("../../.native/core/cli/dist/lib/kind-kubelet-trust.js")'));
  assert.ok(source.includes("nativeMetricsTrust(execute, kindNodeKubeletTrust)"));
  assert.ok(source.includes('process.stderr.write("Native metrics kubelet trust verification failed\\n")'));
  assert.ok(!source.includes("result.stderr"));
  assert.ok(!source.includes("shell: true"));
});
