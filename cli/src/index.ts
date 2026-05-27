#!/usr/bin/env node
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.


/**
 * kars CLI — Enterprise-grade runtime for OpenClaw on Azure.
 *
 * This is the primary entrypoint for the kars command.
 */

import { createCli } from "./cli.js";

const program = createCli();
program.parse(process.argv);
