"use server";

import { BffError, putCredential, reviewCredential } from "@/lib/bff";
import { credentialFormTransition, type CredentialFormState } from "@/lib/credential-review";

export type CredState = CredentialFormState;

export async function putCredentialAction(previous: CredState, form: FormData): Promise<CredState> {
  return credentialFormTransition(previous, form, {
    review: reviewCredential, write: putCredential,
    failure: error => error instanceof BffError ? {
      status: error.status, code: error.code, message: error.message, continuation: error.credentialContinuation,
    } : undefined,
  });
}
