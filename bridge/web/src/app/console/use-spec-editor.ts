// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState } from "react";

function preserved(original: unknown, rebuilt: unknown): boolean {
  if (Array.isArray(original)) {
    return Array.isArray(rebuilt) && original.length === rebuilt.length
      && original.every((value, index) => preserved(value, rebuilt[index]));
  }
  if (original !== null && typeof original === "object") {
    return rebuilt !== null && typeof rebuilt === "object" && !Array.isArray(rebuilt)
      && Object.entries(original).every(([key, value]) =>
        Object.hasOwn(rebuilt, key) && preserved(value, Reflect.get(rebuilt, key)));
  }
  return Object.is(original, rebuilt);
}

export function visualSpec<Form>(
  raw: string,
  parse: (spec: Record<string, unknown> | undefined) => Form,
  build: (form: Form) => Record<string, unknown>,
): { form: Form; error: null } | { form: null; error: string } {
  try {
    const spec: unknown = JSON.parse(raw);
    if (spec === null || typeof spec !== "object" || Array.isArray(spec)) {
      return { form: null, error: "The specification must be a JSON object." };
    }
    const form = parse(Object.fromEntries(Object.entries(spec)));
    if (!preserved(spec, build(form))) {
      return { form: null, error: "Some authored settings cannot be represented without changes in the visual form. Continue editing JSON; your draft is preserved." };
    }
    return { form, error: null };
  } catch (error) {
    return { form: null, error: error instanceof SyntaxError
      ? "Invalid JSON. Correct the draft before switching to the visual form."
      : "These JSON field types cannot be represented in the visual form. Your draft is preserved." };
  }
}

export function useSpecEditor<Form>(
  initialSpec: Record<string, unknown> | undefined,
  parse: (spec: Record<string, unknown> | undefined) => Form,
  build: (form: Form) => Record<string, unknown>,
  jsonOnly = false,
) {
  const [initial] = useState(() => {
    const raw = JSON.stringify(initialSpec ?? {}, null, 2);
    return { raw, ...visualSpec(raw, parse, build) };
  });
  const [f, setF] = useState(() => initial.form ?? parse(undefined));
  const [advanced, setAdvanced] = useState(jsonOnly || initial.error !== null);
  const [rawSpec, setRawSpec] = useState(initial.raw);
  const [modeError, setModeError] = useState(jsonOnly ? null : initial.error);
  function toggleMode() {
    if (jsonOnly) return;
    if (!advanced) {
      setRawSpec(JSON.stringify(build(f), null, 2));
      setModeError(null);
      setAdvanced(true);
      return;
    }
    const result = visualSpec(rawSpec, parse, build);
    setModeError(result.error);
    if (result.error !== null) return;
    setF(result.form);
    setAdvanced(false);
  }
  return {
    f, setF, advanced, rawSpec, setRawSpec, modeError, toggleMode,
    specJson: advanced ? rawSpec : JSON.stringify(build(f)),
  };
}
