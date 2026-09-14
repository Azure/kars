// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Internal fixture adapter, not a CLI command. Build and validate with the
// actual compiled production helpers; never reimplement JS hashes in Python.
let phase = "helper-import";
try {
  const { buildSchemaWriteRequest, verifySchemaWritePreview } = await import(
    "../../../cli/dist/lib/schema-write-request.js");
  const { normalizedCrd, schemaDocuments, schemaIdentity, verifySchemaOwner } = await import(
    "../../../cli/dist/lib/schema-documents.js");
  const { ownedTaskRequest, verifyOwnedTaskState } = await import("./task_schema_owned.mjs");
  if ([buildSchemaWriteRequest, verifySchemaWritePreview, ownedTaskRequest, verifyOwnedTaskState]
    .some(helper => typeof helper !== "function")) {
    throw new Error("Required production helper exports are missing");
  }
  const mode = process.argv[2];
  phase = "mode";
  if (process.argv.length !== 3 || !["check", "build", "validate", "owned-request", "owned-check"].includes(mode)) {
    throw new Error("Unsupported internal fixture mode");
  }
  if (mode === "check") {
    process.stdout.write(JSON.stringify({ ready: true }));
  } else {
    phase = "input";
    const chunks = [];
    let bytes = 0;
    for await (const chunk of process.stdin) {
      bytes += chunk.length;
      if (bytes > 8 * 1024 * 1024) throw new Error("Fixture input exceeds its bound");
      chunks.push(chunk);
    }
    const input = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    if (mode === "owned-request" || mode === "owned-check") {
      phase = mode;
      process.stdout.write(JSON.stringify(mode === "owned-request" ? ownedTaskRequest(input) : verifyOwnedTaskState(input)));
    } else {
      phase = "target";
      if (typeof input.rendered !== "string") throw new Error("Missing rendered chart");
      const documents = schemaDocuments(input.rendered);
      if (documents.length !== 1) throw new Error("Exactly one Task CRD is required");
      const desired = documents[0];
      normalizedCrd(desired);
      if (desired.metadata.name !== "karstasks.kars.azure.com" || desired.spec.names.kind !== "KarsTask"
        || ["uid", "resourceVersion", "ownerReferences", "namespace"].some(key => key in desired.metadata)) {
        throw new Error("Unreviewed Task chart identity");
      }
      const owner = { namespace: "kars-system", release: "kars", ownership: "helm" };
      phase = "current-owner";
      normalizedCrd(input.current);
      verifySchemaOwner(input.current, owner);
      if (input.current.metadata.name !== desired.metadata.name) throw new Error("Another current CRD");
      const currentIdentity = schemaIdentity(input.current);
      if (mode === "build") {
        phase = "build";
        const request = buildSchemaWriteRequest(desired, input.current, owner);
        // Keep the JSON payload as a string: Python must not reserialize numbers.
        process.stdout.write(JSON.stringify({ args: request.args, input: JSON.stringify(request.object) }));
      } else {
        phase = "validate";
        if (typeof input.returned !== "string") throw new Error("Missing raw API response");
        const checked = JSON.parse(input.returned);
        verifySchemaWritePreview(checked, desired, owner, currentIdentity.uid);
        if (schemaIdentity(checked).resourceVersion !== currentIdentity.resourceVersion) {
          throw new Error("Task preview changed its reviewed resourceVersion");
        }
        process.stdout.write(JSON.stringify({ validated: true }));
      }
    }
  }
} catch {
  console.error(`SRE-TASK-PAYLOAD-FAIL ${phase}`);
  process.exitCode = 1;
}
