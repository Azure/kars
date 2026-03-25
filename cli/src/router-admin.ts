/**
 * Router admin token helper.
 *
 * Reads the per-sandbox admin token from the K8s secret `router-admin-token`
 * and provides helpers for authenticated curl calls to the inference router.
 */

import { execa } from "execa";

/**
 * Read the admin token for a sandbox from the K8s secret.
 * Returns empty string if the secret doesn't exist (backwards-compatible).
 */
export async function getAdminToken(namespace: string): Promise<string> {
  try {
    const { stdout } = await execa("kubectl", [
      "get", "secret", "router-admin-token",
      "-n", namespace,
      "-o", "jsonpath={.data.token}",
    ], { stdio: "pipe" });
    if (stdout) {
      return Buffer.from(stdout, "base64").toString("utf-8");
    }
  } catch {
    // Secret doesn't exist — router running without admin token (dev mode)
  }
  return "";
}

/**
 * Build curl args with optional admin token Authorization header.
 */
export function withAdminAuth(curlArgs: string[], adminToken: string): string[] {
  if (adminToken) {
    // Insert auth header before the URL (which is always last)
    const urlIdx = curlArgs.length - 1;
    const url = curlArgs[urlIdx];
    return [
      ...curlArgs.slice(0, urlIdx),
      "-H", `Authorization: Bearer ${adminToken}`,
      url,
    ];
  }
  return curlArgs;
}
