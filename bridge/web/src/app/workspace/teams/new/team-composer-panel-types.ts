// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Dispatch, SetStateAction } from "react";
import type { Options } from "@/lib/types";

export type Role = { id: number; name: string; system_prompt: string; runtime: string; model: string; skills: string[]; resources?: import("@/lib/types/teams").TeamRoleResources | null };

export interface GovernancePanelInput {
  name: string;
  options: Options;
  mcp: string[];
  setMcp: Dispatch<SetStateAction<string[]>>;
  toolPolicy: string;
  setToolPolicy: Dispatch<SetStateAction<string>>;
  commons: string;
  setCommons: Dispatch<SetStateAction<string>>;
  memory: string;
  setMemory: Dispatch<SetStateAction<string>>;
  selectedMemoryOption: Options["memories"][number] | null;
  runtime: string;
  setRuntime: Dispatch<SetStateAction<string>>;
  model: string;
  setModel: Dispatch<SetStateAction<string>>;
  modelFallbacks: string[];
  setModelFallbacks: Dispatch<SetStateAction<string[]>>;
  egressText: string;
  setEgressText: Dispatch<SetStateAction<string>>;
}

export interface OrgPanelInput {
  resources: import("@/lib/types/teams").TeamRoleResources;
  name: string;
  tier: number;
  authorityCeiling: string;
  delegationDepth: string;
  roles: Role[];
  options: Options;
  addFromArchetype: (id: string) => void;
  addRole: () => void;
  addNote: string | null;
  patchRole: (id: number, patch: Partial<Role>) => void;
  removeRole: (id: number) => void;
}
