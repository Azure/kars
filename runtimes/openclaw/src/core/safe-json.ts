// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Small helper used by AGT tool registrations — pretty-prints + size-caps.
// Extracted from plugin.ts in S15.f.8 to make tool modules under
// `core/agt-tools/` self-contained.

export function safeJson(obj: unknown, maxLen = 8000): string {
  try {
    const s = JSON.stringify(obj, null, 2);
    return s.length > maxLen ? s.slice(0, maxLen) + "\n...(truncated)" : s;
  } catch {
    return String(obj).slice(0, maxLen);
  }
}
