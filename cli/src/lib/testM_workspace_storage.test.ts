// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { buildWorkspaceStorageSpec } from "./workspace-storage.js";

describe("buildWorkspaceStorageSpec", () => {
  it("omits storage when persistence is not requested", () => {
    expect(buildWorkspaceStorageSpec({})).toBeUndefined();
  });

  it("builds a retained dynamic workspace", () => {
    expect(
      buildWorkspaceStorageSpec({
        workspaceStorage: "20Gi",
        workspaceStorageClass: "managed-csi",
        workspaceRetainPolicy: "Retain",
      }),
    ).toEqual({
      workspace: {
        size: "20Gi",
        storageClassName: "managed-csi",
        accessModes: ["ReadWriteOnce"],
        retainPolicy: "Retain",
      },
    });
  });

  it("builds a destructive dynamic workspace only when requested", () => {
    expect(
      buildWorkspaceStorageSpec({
        workspaceStorage: "1.5Gi",
        workspaceRetainPolicy: "Delete",
      }),
    ).toEqual({
      workspace: {
        size: "1.5Gi",
        accessModes: ["ReadWriteOnce"],
        retainPolicy: "Delete",
      },
    });
  });

  it("builds a pure existing-claim reference", () => {
    expect(
      buildWorkspaceStorageSpec({ workspaceExistingClaim: "restored-workspace" }),
    ).toEqual({ workspace: { existingClaim: "restored-workspace" } });
  });
});