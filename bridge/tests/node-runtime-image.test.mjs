// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Script } from "node:vm";
import test from "node:test";

function dockerfile(component) {
  const source = readFileSync(new URL(`../${component}/Dockerfile`, import.meta.url), "utf8");
  const instructions = source
    .replace(/\\\r?\n/g, " ")
    .split(/\r?\n/)
    .map(line => line.trim())
    .filter(line => line && !line.startsWith("#"));
  const runtimeStart = instructions.findLastIndex(line => /^FROM /i.test(line));
  assert.ok(runtimeStart >= 0, `${component} must declare a runtime base`);
  const stages = new Map();
  for (const instruction of instructions.filter(line => /^FROM /i.test(line))) {
    const [, base, name] = /^FROM (\S+)(?: AS (\S+))?$/i.exec(instruction);
    if (name) stages.set(name, stages.get(base) ?? base);
  }
  return {
    instructions,
    build: instructions.slice(0, runtimeStart).join("\n"),
    runtime: instructions.slice(runtimeStart),
    runtimeBase: stages.get(instructions[runtimeStart].split(/\s+/)[1])
      ?? instructions[runtimeStart].split(/\s+/)[1],
  };
}

function execArguments(instructions, name) {
  const instruction = instructions.findLast(line => line.startsWith(`${name} `));
  return instruction ? JSON.parse(instruction.slice(name.length).trim()) : [];
}

for (const component of ["web", "teams-gateway"]) {
  test(`${component}: shipping stage uses official Azure Linux distroless, not a Debian or debug runtime`, () => {
    const { runtime, runtimeBase } = dockerfile(component);
    assert.match(
      runtimeBase,
      /^mcr\.microsoft\.com\/azurelinux\/distroless\/base:3\.0(?:@sha256:[a-f0-9]{64})?$/,
    );
    assert.equal(runtime.findLast(line => /^USER /i.test(line)), "USER 10001:10001");
    assert.ok(runtime.includes("WORKDIR /app"));
  });

  test(`${component}: only Node is executed in the shell-free shipping stage`, () => {
    const { runtime } = dockerfile(component);
    const entrypoint = execArguments(runtime, "ENTRYPOINT");
    assert.ok(Array.isArray(entrypoint) && entrypoint.length > 0);
    assert.match(entrypoint[0], /^(?:\/(?:usr\/(?:local\/)?)?bin\/)?node$/);
    assert.deepEqual(
      [...entrypoint.slice(1), ...execArguments(runtime, "CMD")],
      [component === "web" ? "server.js" : "dist/main.js"],
    );
    for (const instruction of runtime.filter(line => /^RUN /i.test(line))) {
      const command = JSON.parse(instruction.slice(4));
      assert.ok(Array.isArray(command) && command.length > 0);
      assert.match(command[0], /^(?:\/(?:usr\/(?:local\/)?)?bin\/)?node$/);
    }
    assert.doesNotMatch(runtime.join("\n"), /\b(?:NODE_TLS_REJECT_UNAUTHORIZED=0|--use-bundled-ca)\b/);
  });

  test(`${component}: Node22 dependencies are built outside the runtime without importing global tooling`, () => {
    const { build, runtime } = dockerfile(component);
    assert.match(build, /^FROM node:22-bookworm-slim(?: AS \S+)?$/m);
    assert.match(runtime.join("\n"), /^COPY --from=(?:build|builder) \/usr\/local\/bin\/node \/usr\/local\/bin\/node$/m);
    assert.match(runtime.join("\n"), /^COPY --from=(?:build|builder) \/usr\/local\/LICENSE \/usr\/local\/share\/licenses\/node\/LICENSE$/m);
    for (const instruction of runtime.filter(line => /^COPY /i.test(line))) {
      if (instruction.includes(" /usr/")) {
        assert.match(instruction, /^COPY --from=(?:build|builder) \/usr\/local\/(?:bin\/node \/usr\/local\/bin\/node|LICENSE \/usr\/local\/share\/licenses\/node\/LICENSE)$/);
      }
      assert.doesNotMatch(instruction, /\/(?:npm|npx|corepack)(?:\/|\s|$)/);
    }
    assert.doesNotMatch(runtime.join("\n"), /\b(?:npm|npx|apt-get|tdnf|useradd|groupadd)\b/);
  });

  test(`${component}: Microsoft libstdc++ is exported by RPM ownership without losing base package inventory`, () => {
    const { build, runtime } = dockerfile(component);
    assert.match(build, /^FROM mcr\.microsoft\.com\/azurelinux\/base\/core:3\.0 AS runtime-libs$/m);
    assert.match(build, /^COPY --from=runtime-base \/var\/lib\/rpmmanifest\/ \/runtime-libs\/var\/lib\/rpmmanifest\/$/m);
    assert.match(build, /rpm -V libstdc\+\+/);
    assert.match(build, /for package in glibc libgcc;/);
    assert.match(build, /grep -Fx "\$\(rpm -q "\$package"\)"/);
    assert.match(build, /rpm -ql libstdc\+\+ > \/package-files/);
    assert.match(build, /cp -a "\$path" "\/runtime-libs\$directory\/"/);
    assert.match(build, /rpm -q libstdc\+\+ >> \/runtime-libs\/var\/lib\/rpmmanifest\/container-manifest-1/);
    assert.match(build, /%\{SOURCERPM\}\\n' libstdc\+\+\s+>> \/runtime-libs\/var\/lib\/rpmmanifest\/container-manifest-2/);
    assert.ok(runtime.includes("COPY --from=runtime-libs /runtime-libs/ /"));
    assert.doesNotMatch(runtime.join("\n"), /\/var\/lib\/rpm(?:\/|\s)|\/rpmdb/);
  });

  test(`${component}: final Microsoft runtime executes Node22 and parses the populated OS CA bundle`, () => {
    const { runtime } = dockerfile(component);
    assert.match(runtime.join("\n"), /^ENV .*NODE_EXTRA_CA_CERTS=\/etc\/pki\/tls\/certs\/ca-bundle\.crt$/m);
    const setup = runtime.filter(line => /^RUN /i.test(line))
      .map(line => JSON.parse(line.slice(4)))
      .find(command => command[2]?.includes("Node22 required"));
    assert.ok(setup);
    assert.match(setup[2], /process\.versions\.node\.split\('\.'\)\[0\] !== '22'/);
    assert.match(setup[2], /createSecureContext\(\{ ca: fs\.readFileSync\(process\.env\.NODE_EXTRA_CA_CERTS\) \}\)/);
    assert.match(setup[2], /fs\.chownSync\([^;]*10001, 10001\)/);
  });

  test(`${component}: embedded runtime checks remain valid JavaScript after Docker JSON decoding`, () => {
    const { runtime } = dockerfile(component);
    for (const instruction of runtime.filter(line => /^(?:RUN|HEALTHCHECK) /i.test(line))) {
      const command = JSON.parse(instruction.startsWith("RUN ")
        ? instruction.slice(4)
        : instruction.split(/\sCMD\s/, 2)[1]);
      const scriptIndex = command.indexOf("-e") + 1;
      assert.ok(scriptIndex > 0);
      const source = command[scriptIndex];
      assert.equal(typeof source, "string");
      assert.doesNotThrow(() => new Script(command.includes("--input-type=module")
        ? `(async () => { ${source} })`
        : source));
    }
  });
}

test("web: preserve the complete standalone trace, static assets, and native optional dependencies", () => {
  const { build, runtime } = dockerfile("web");
  assert.match(build, /^RUN npm ci$/m);
  assert.match(build, /^RUN npm run build$/m);
  const copy = runtime.filter(line => /^COPY /i.test(line)).join("\n");
  assert.match(copy, /--from=build (?:--chown=\S+ )?\/app\/\.next\/standalone \.\/$/m);
  assert.match(copy, /--from=build (?:--chown=\S+ )?\/app\/\.next\/static \.\/\.next\/static$/m);
  assert.match(copy, /--from=build (?:--chown=\S+ )?\/app\/public \.\/public$/m);
  assert.match(runtime.join("\n"), /^ENV .*NODE_ENV=production.*NEXT_TELEMETRY_DISABLED=1.*PORT=3000$/m);
  assert.ok(runtime.includes("EXPOSE 3000"));
});

test("web: the final nonroot runtime exercises sharp encoding and decoding with a writable cache", () => {
  const { runtime } = dockerfile("web");
  assert.match(runtime.join("\n"), /\/app\/\.next\/cache/);
  const userIndex = runtime.indexOf("USER 10001:10001");
  const nativeProbe = runtime.slice(userIndex + 1)
    .filter(line => /^RUN /i.test(line))
    .map(line => JSON.parse(line.slice(4)))
    .find(command => command[2]?.includes("sharp runtime verification failed"));
  assert.ok(nativeProbe);
  assert.match(nativeProbe[2], /createRequire\(require\.resolve\('next\/package\.json'\)\)\('sharp'\)/);
  assert.match(nativeProbe[2], /\.png\(\)\.toBuffer\(\)/);
  assert.match(nativeProbe[2], /sharp\(data\)\.metadata\(\)/);
  assert.match(nativeProbe[2], /process\.exit\(1\)/);
});

test("teams-gateway: production-only dependency installation stays in a discarded build stage", () => {
  const { build, runtime } = dockerfile("teams-gateway");
  assert.match(build, /\bnpm ci --omit=dev --ignore-scripts\b/);
  assert.match(runtime.join("\n"), /^COPY --from=\S+ (?:--chown=\S+ )?\/app\/node_modules \.\/node_modules$/m);
  assert.match(runtime.join("\n"), /^COPY --from=builder (?:--chown=\S+ )?\/app\/dist \.\/dist$/m);
  assert.ok(runtime.includes("ENV NODE_ENV=production"));
  assert.ok(runtime.includes("EXPOSE 3978 3979"));
  assert.ok(runtime.includes('RUN ["node", "--input-type=module", "-e", "await import(\'./dist/main.js\');"]'));
});

test("teams-gateway: Docker health checks use exec-form Node without requiring a shell", () => {
  const { runtime } = dockerfile("teams-gateway");
  const healthcheck = runtime.find(line => /^HEALTHCHECK /i.test(line));
  assert.ok(healthcheck);
  const command = JSON.parse(healthcheck.split(/\sCMD\s/, 2)[1]);
  assert.ok(Array.isArray(command));
  assert.match(command[0], /^(?:\/(?:usr\/(?:local\/)?)?bin\/)?node$/);
  assert.equal(command[1], "-e");
  assert.match(command[2], /http:\/\/(?:localhost|127\.0\.0\.1):3979\/healthz/);
  assert.match(command[2], /process\.exit\(1\)/);
});
