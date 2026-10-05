// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { request } from "node:http";
import { routerUrl } from "./router-client.js";

/** Only a positive, well-formed local-router policy decision authorizes execution. */
export async function authorizeTaskAction(action: string, context: Record<string, unknown>): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const body = JSON.stringify({ action, context });
    const req = request(routerUrl("/agt/evaluate"), {
      method: "POST", timeout: 2000,
      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) },
    }, (res) => {
      let data = "";
      res.on("data", (chunk: Buffer) => {
        data += chunk.toString();
        if (Buffer.byteLength(data) > 65536) { res.destroy(); resolve(false); }
      });
      res.on("end", () => {
        if (res.statusCode !== 200) return resolve(false);
        try { resolve(JSON.parse(data)?.allowed === true); } catch { resolve(false); }
      });
      res.on("error", () => resolve(false));
      res.on("aborted", () => resolve(false));
    });
    req.on("error", () => resolve(false));
    req.on("timeout", () => { req.destroy(); resolve(false); });
    req.end(body);
  }).catch(() => false);
}
