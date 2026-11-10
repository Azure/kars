// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { withOperatorJournal } from "./operator-journal.js";
import type { RootMigrationJournal, RootMigrationReceipt } from "./private-root-migration.js";

/** Retains the private migration API over the shared append-only operator journal. */
export function withRootMigrationJournal<T>(
  file: string, create: boolean,
  action: (journal: RootMigrationJournal, current?: RootMigrationReceipt) => Promise<T>,
): Promise<T> {
  return withOperatorJournal<RootMigrationReceipt, T>(file, create, action);
}
