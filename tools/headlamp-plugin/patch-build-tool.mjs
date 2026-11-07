// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

const oldChecksums = `github.com/rabbitmq/amqp091-go v1.3.4 h1:tXuIslN1nhDqs2t6Jrz3BAoqvt4qIZzxvdbdcxWtHYU=
github.com/rabbitmq/amqp091-go v1.3.4/go.mod h1:ogQDLSOACsLPsIq0NpbtiifNZi2YOz0VTJ0kHRghqbM=
`;
// Published module and go.mod hashes from sum.golang.org, not synthetic lock metadata.
const newChecksums = `github.com/rabbitmq/amqp091-go v1.14.0 h1:RSaT7aOKt/OrkVUyswPDW29lnRz9psuGmfZFBmLqLek=
github.com/rabbitmq/amqp091-go v1.14.0/go.mod h1:Hy4jKW5kQART1u+JkDTF9YYOQUHXqMuhrgxOEeS7G4o=
`;
const testChecksums = [
  "go.uber.org/goleak v1.3.0 h1:2K3zAYmnTNqV73imy9J1T3WC+gmCePx2hEGkimedGto=",
  "go.uber.org/goleak v1.3.0/go.mod h1:CoHD4mav9JJNrW/WLlf7HGZPjdw8EucARQHekz1X6bE=",
];

function replaceOnce(text, before, after, label) {
  if (text.includes(after)) {
    assert(!text.includes(before) && text.split(after).length === 2, `Unexpected patched ${label}`);
    return text;
  }
  assert.equal(text.split(before).length, 2, `Upstream ${label} changed`);
  return text.replace(before, after);
}

export function patchPluginFixtures(directory) {
  const manifest = JSON.parse(readFileSync(path.join(directory, "package.json"), "utf8"));
  assert.equal(manifest.name, "@kinvolk/headlamp-plugin");
  assert.equal(manifest.version, "0.13.1", "Review the fixture patch when updating the locked plugin build tool");
  const updates = [];
  for (const name of ["consume", "publish"]) {
    const root = path.join(directory, "official-plugins/keda/test-files", name);
    const modFile = path.join(root, "go.mod");
    const sumFile = path.join(root, "go.sum");
    let mod = readFileSync(modFile, "utf8");
    assert(mod.startsWith(`module github.com/headlamp-k8s/plugins/keda/${name}\n`), "Unexpected fixture module");
    mod = replaceOnce(mod, "\ngo 1.18\n", "\ngo 1.20\n", `${name} Go version`);
    mod = replaceOnce(mod, "require github.com/rabbitmq/amqp091-go v1.3.4\n", "require github.com/rabbitmq/amqp091-go v1.14.0\n", `${name} RabbitMQ requirement`);
    const sum = replaceOnce(readFileSync(sumFile, "utf8"), oldChecksums, newChecksums, `${name} RabbitMQ checksums`);
    const lines = sum.trimEnd().split("\n");
    for (const checksum of testChecksums) {
      const prefix = checksum.slice(0, checksum.indexOf(" h1:"));
      const matching = lines.filter(line => line.startsWith(prefix + " "));
      assert(matching.length === 0 || (matching.length === 1 && matching[0] === checksum), "Unexpected goleak checksum");
      if (!matching.length) lines.push(checksum);
    }
    updates.push([modFile, mod], [sumFile, lines.sort().join("\n") + "\n"]);
  }
  // Validate both upstream modules before changing either one.
  for (const [file, content] of updates) writeFileSync(file, content);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const require = createRequire(import.meta.url);
  patchPluginFixtures(path.dirname(require.resolve("@kinvolk/headlamp-plugin/package.json")));
}
