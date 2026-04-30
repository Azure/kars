#!/usr/bin/env node
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.


/**
 * AzureClaw CLI — Enterprise-grade runtime for OpenClaw on Azure.
 *
 * This is the primary entrypoint for the azureclaw command.
 */

import { createCli } from "./cli.js";

const program = createCli();
program.parse(process.argv);
