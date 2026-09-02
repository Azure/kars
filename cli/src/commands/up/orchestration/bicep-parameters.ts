// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export interface BicepParameterOptions {
  location: string;
  baseName: string;
  recoverKeyVault?: boolean;
  vmSize: string;
  systemVmSize: string;
  kataVmSize: string;
  kubernetesVersion: string;
  systemNodeCount: number;
  nodeCount: number;
  kataNodeCount: number;
  systemPoolName: string;
  sandboxPoolName: string;
  kataPoolName: string;
}

export interface PoolNames {
  systemPoolName: string;
  sandboxPoolName: string;
  kataPoolName: string;
}

export interface ProjectedBicepParameterOptions {
  location: string;
  baseName: string;
  recoverKeyVault?: boolean;
  nodeVmSize?: string;
  systemVmSize?: string;
  kataVmSize?: string;
  kubernetesVersion?: string;
  systemNodeCount?: number;
  nodeCount?: number;
  kataNodeCount?: number;
  systemPoolName?: string;
  sandboxPoolName?: string;
  kataPoolName?: string;
}


export function parsePositiveInteger(value: string): number {
  if (!/^[1-9]\d*$/.test(value)) {
    throw new Error("must be an integer from 1 to 100");
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed > 100) {
    throw new Error("must be an integer from 1 to 100");
  }
  return parsed;
}

export function validateInfrastructureMode(options: {
  skipInfra: boolean;
  forceInfra: boolean;
}): void {
  if (options.skipInfra && options.forceInfra) {
    throw new Error(
      "--skip-infra and --force-infra cannot be used together",
    );
  }
}

export function resolvePoolNames(options: {
  systemPoolName?: string;
  sandboxPoolName?: string;
  kataPoolName?: string;
}): PoolNames {
  return {
    systemPoolName: options.systemPoolName ?? "system",
    sandboxPoolName: options.sandboxPoolName ?? "clawpool",
    kataPoolName: options.kataPoolName ?? "katapool",
  };
}


export function buildBicepParameters(
  options: BicepParameterOptions,
): string[] {
  if (!options.kubernetesVersion.trim()) {
    throw new Error("Preflight did not resolve a Kubernetes version");
  }
  if (!Number.isSafeInteger(options.nodeCount) || options.nodeCount <= 0) {
    throw new Error("Preflight did not resolve a positive node count");
  }
  if (
    !Number.isSafeInteger(options.systemNodeCount) ||
    options.systemNodeCount <= 0
  ) {
    throw new Error("Preflight did not resolve a positive system node count");
  }
  if (!options.kataVmSize.trim()) {
    throw new Error("Preflight did not resolve a Kata VM size");
  }
  if (
    !Number.isSafeInteger(options.kataNodeCount) ||
    options.kataNodeCount < 0
  ) {
    throw new Error("Preflight did not resolve a non-negative Kata node count");
  }
  return [
    `location=${options.location}`,
    `baseName=${options.baseName}`,
    `recoverKeyVault=${options.recoverKeyVault === true}`,
    `vmSize=${options.vmSize}`,
    `systemVmSize=${options.systemVmSize}`,
    `kataVmSize=${options.kataVmSize}`,
    `kubernetesVersion=${options.kubernetesVersion}`,
    `systemNodeCount=${options.systemNodeCount}`,
    `nodeCount=${options.nodeCount}`,
    `kataNodeCount=${options.kataNodeCount}`,
    `systemPoolName=${options.systemPoolName}`,
    `sandboxPoolName=${options.sandboxPoolName}`,
    `kataPoolName=${options.kataPoolName}`,
  ];
}

export function buildProjectedBicepParameters(
  options: ProjectedBicepParameterOptions,
): string[] {
  const vmSize = options.nodeVmSize?.trim();
  const systemVmSize = options.systemVmSize?.trim();
  const kataVmSize = options.kataVmSize?.trim();
  if (!vmSize || !systemVmSize || !kataVmSize) {
    throw new Error(
      "Preflight did not resolve sandbox, system, and Kata VM sizes",
    );
  }

  return buildBicepParameters({
    location: options.location,
    baseName: options.baseName,
    recoverKeyVault: options.recoverKeyVault,
    vmSize,
    systemVmSize,
    kataVmSize,
    kubernetesVersion: options.kubernetesVersion ?? "",
    systemNodeCount: options.systemNodeCount ?? Number.NaN,
    nodeCount: options.nodeCount ?? Number.NaN,
    kataNodeCount: options.kataNodeCount ?? Number.NaN,
    ...resolvePoolNames(options),
  });
}

