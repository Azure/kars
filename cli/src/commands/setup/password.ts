// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/** Also embedded in the private setup page so browser and server agree. */
export function setupPasswordIssue(password: string): string {
  if (password.length < 12 || new TextEncoder().encode(password).length > 72) return "Use at least 12 characters and no more than 72 UTF-8 bytes.";
  if (!/[a-z]/.test(password) || !/[A-Z]/.test(password) || !/[0-9]/.test(password) || !/[^A-Za-z0-9\s]/.test(password)) {
    return "Include an uppercase letter, a lowercase letter, a number and a symbol.";
  }
  return "";
}
