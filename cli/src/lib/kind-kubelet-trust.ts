// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { X509Certificate } from "node:crypto";
import { isIPv4 } from "node:net";

type ObjectMap = Record<string, any>;

export function kindNodeKubeletTrust(cluster: string, node: ObjectMap, container: ObjectMap, pem: string, now = Date.now()) {
  const hostname = node.metadata?.name;
  const suffix = typeof hostname === "string" && hostname.startsWith(`${cluster}-`) ? hostname.slice(cluster.length + 1) : "";
  const role = /^(control-plane|worker)(?:[1-9][0-9]*)?$/.exec(suffix)?.[1];
  const addresses = node.status?.addresses ?? [];
  const ips = addresses.filter((address: ObjectMap) => address.type === "InternalIP");
  const names = addresses.filter((address: ObjectMap) => address.type === "Hostname");
  if (!/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(cluster) || !role || !node.metadata?.uid
    || ips.length !== 1 || !isIPv4(ips[0].address) || names.length !== 1 || names[0].address !== hostname
    || container.Name?.replace(/^\//, "") !== hostname || container.State?.Running !== true
    || container.Config?.Labels?.["io.x-k8s.kind.cluster"] !== cluster
    || container.Config?.Labels?.["io.x-k8s.kind.role"] !== role
    || !Object.values(container.NetworkSettings?.Networks ?? {}).some((network: any) => network.IPAddress === ips[0].address)) {
    throw new Error("Kind kubelet trust requires the owned node container and matching node address");
  }
  const blocks = pem.match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g) ?? [];
  if (pem.length > 65536 || blocks.length !== 2 || blocks.reduce((rest, cert) => rest.replace(cert, ""), pem).trim()) {
    throw new Error("Kind kubelet trust requires the public serving certificate and its CA only");
  }
  const [leaf, ca] = blocks.map(cert => new X509Certificate(cert));
  if (leaf.ca || !ca.ca || !leaf.checkIssued(ca) || !leaf.verify(ca.publicKey)
    || !ca.checkIssued(ca) || !ca.verify(ca.publicKey)
    || leaf.checkHost(hostname, { subject: "never", wildcards: false }) !== hostname
    || [leaf, ca].some(cert => now < Date.parse(cert.validFrom) || now >= Date.parse(cert.validTo))) {
    throw new Error("Kind kubelet certificate chain, hostname or validity does not match this installation");
  }
  return { hostAliases: [{ ip: ips[0].address, hostnames: [hostname] }],
    kubelet: { insecureTLS: false, preferredAddressTypes: ["Hostname"], caBundle: ca.toString() } };
}

export function kindBetaKubeletTrust(cluster: string, node: ObjectMap, container: ObjectMap, pem: string, now = Date.now()) {
  if (node.metadata?.name !== `${cluster}-control-plane`) {
    throw new Error("Kind kubelet trust requires the owned control-plane container and matching node address");
  }
  return kindNodeKubeletTrust(cluster, node, container, pem, now);
}
