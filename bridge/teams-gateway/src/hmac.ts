// kars Bridge — Teams Gateway: HMAC-SHA256 authentication for internal endpoints.

import { createHmac, timingSafeEqual } from "node:crypto";

export const SIGNATURE_HEADER = "x-teams-internal-signature";

export function computeHmac(secret: string, body: string): string {
  return createHmac("sha256", secret).update(body).digest("hex");
}

export function verifyHmac(
  secret: string,
  body: string,
  signature: string | null
): boolean {
  if (!signature || signature.length !== 64) return false;
  const expected = computeHmac(secret, body);
  try {
    return timingSafeEqual(
      Buffer.from(expected, "hex"),
      Buffer.from(signature, "hex")
    );
  } catch {
    return false;
  }
}
