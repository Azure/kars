#!/usr/bin/env node

/**
 * AzureClaw CLI — Enterprise-grade runtime for OpenClaw on Azure.
 *
 * This is the primary entrypoint for the azureclaw command.
 */

import { createCli } from "./cli.js";

const program = createCli();
program.parse(process.argv);
