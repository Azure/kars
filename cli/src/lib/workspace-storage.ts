// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export interface WorkspaceStorageOptions {
  workspaceStorage?: string;
  workspaceStorageClass?: string;
  workspaceExistingClaim?: string;
  workspaceRetainPolicy?: "Retain" | "Delete";
}

export function buildWorkspaceStorageSpec(
  options: WorkspaceStorageOptions,
): Record<string, unknown> | undefined {
  if (options.workspaceExistingClaim) {
    return {
      workspace: { existingClaim: options.workspaceExistingClaim },
    };
  }
  if (!options.workspaceStorage) return undefined;
  return {
    workspace: {
      size: options.workspaceStorage,
      ...(options.workspaceStorageClass
        ? { storageClassName: options.workspaceStorageClass }
        : {}),
      accessModes: ["ReadWriteOnce"],
      retainPolicy: options.workspaceRetainPolicy ?? "Retain",
    },
  };
}