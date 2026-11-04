// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export const NATIVE_MISSION_SESSION_PREFIX = "agent:main:kars-mission:";

export function createSystemPromptHook(systemPrompt: string) {
  return (_event?: unknown, context?: { sessionKey?: string }) => {
    // Replacing the native prompt also discards its reviewed phase and required outputs.
    return context?.sessionKey?.startsWith(NATIVE_MISSION_SESSION_PREFIX)
      ? { prependSystemContext: systemPrompt }
      : { systemPrompt };
  };
}
