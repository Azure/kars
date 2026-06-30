#!/usr/bin/env node
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.


/**
 * kars CLI — Enterprise-grade runtime for OpenClaw on Azure.
 *
 * This is the primary entrypoint for the kars command.
 */

import { createCli } from "./cli.js";
import { bootstrapKubeContext } from "./lib/kube-bootstrap.js";
import { maybeNotifyCliUpdate } from "./lib/update-check.js";

await bootstrapKubeContext(process.argv);
const program = createCli();
await program.parseAsync(process.argv);

// Passive, best-effort: tell the user if a newer CLI has been published. Runs
// after the command so it never delays or clutters real output, is cached to
// at most one network check per day, and stays silent in CI / non-TTY / when
// opted out (KARS_NO_UPDATE_CHECK=1). The interactive install lives in
// `kars update`. Skip when the user already ran `kars update`.
if (!process.argv.slice(2).includes("update")) {
  await maybeNotifyCliUpdate();
}
