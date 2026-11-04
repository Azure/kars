// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { Command } from "commander";
import { execa } from "execa";
import { applyDispatcherImage, reviewDispatcherImage, verifyDispatcherImage, type DispatcherImageReceipt } from "../lib/dispatcher-image.js";
import { withOperatorJournal } from "../lib/operator-journal.js";
import type { Execute } from "../lib/private-activation.js";
import { privateCommandFailure } from "../lib/private-activation-command-diagnostics.js";

const execute = (context: string): Execute => async (args, input) => {
  try {
    const result = await execa("kubectl", ["--context", context, ...args], { stdio: "pipe", ...(input ? { input } : {}) });
    return result.stdout;
  } catch (error) {
    const failure = privateCommandFailure(error, args);
    failure.message = `Dispatcher image ${failure.facts.operation} failed; no automatic retry. ${failure.message}`;
    throw failure;
  }
};

export function dispatcherImageCommand(): Command {
  const command = new Command("dispatcher-image")
    .description("Review/apply one immutable image for the existing non-private Helm-owned mission dispatcher; never publish Core");
  command.command("review")
    .requiredOption("--context <context>")
    .requiredOption("--namespace <namespace>")
    .requiredOption("--release <release>")
    .requiredOption("--image <reference>")
    .requiredOption("--receipt <file>", "New private append-only review/attempt receipt")
    .action(async options => {
      await withOperatorJournal<DispatcherImageReceipt, void>(options.receipt, true, async journal => {
        const review = await reviewDispatcherImage(execute(options.context), options.context, options.namespace, options.release, options.image);
        await journal.save(review);
        console.log(JSON.stringify({ phase: review.phase, reviewDigest: review.reviewDigest, receipt: options.receipt }));
      });
    });
  for (const operation of ["apply", "verify"] as const) {
    command.command(operation).argument("<receipt>")
      .description(operation === "apply" ? "Apply the exact reviewed image once and verify its rollout" : "Read-only verification of an attempted image update; never replay the patch")
      .action(async (file: string) => {
        await withOperatorJournal<DispatcherImageReceipt, void>(file, false, async (journal, receipt) => {
          if (!receipt?.review?.context) throw new Error("Dispatcher image receipt has no bound context");
          const run = operation === "apply" ? applyDispatcherImage : verifyDispatcherImage;
          const result = await run(execute(receipt.review.context), receipt, journal);
          console.log(JSON.stringify({ phase: result.phase, reviewDigest: result.reviewDigest, observed: result.observed, receipt: file }));
        });
      });
  }
  return command;
}
