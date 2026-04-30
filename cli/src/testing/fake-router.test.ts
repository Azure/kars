// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { FakeRouter } from "./fake-router.js";

describe("FakeRouter", () => {
  it("serves inline JSON fixtures and records requests", async () => {
    const router = await FakeRouter.start({
      routes: [
        {
          method: "POST",
          path: "/v1/chat/completions",
          body: {
            choices: [{ message: { role: "assistant", content: "hi" } }],
            usage: { total_tokens: 3 },
          },
        },
      ],
    });

    try {
      const res = await fetch(`${router.baseUrl}/v1/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ messages: [{ role: "user", content: "x" }] }),
      });
      expect(res.status).toBe(200);
      const json = (await res.json()) as { usage: { total_tokens: number } };
      expect(json.usage.total_tokens).toBe(3);

      expect(router.log).toHaveLength(1);
      expect(router.log[0].method).toBe("POST");
      expect(router.log[0].path).toBe("/v1/chat/completions");
      const recorded = JSON.parse(router.log[0].body.toString("utf8"));
      expect(recorded.messages[0].content).toBe("x");
    } finally {
      await router.stop();
    }
  });

  it("returns a 404 with a structured error for unknown routes", async () => {
    const router = await FakeRouter.start({ routes: [] });
    try {
      const res = await fetch(`${router.baseUrl}/nope`);
      expect(res.status).toBe(404);
      const body = (await res.json()) as { error: { code: string } };
      expect(body.error.code).toBe("fake_router_no_fixture");
    } finally {
      await router.stop();
    }
  });

  it("supports dynamic body functions for per-request responses", async () => {
    let n = 0;
    const router = await FakeRouter.start({
      routes: [
        {
          method: "GET",
          path: "/count",
          body: () => ({ n: ++n }),
        },
      ],
    });
    try {
      const a = (await (await fetch(`${router.baseUrl}/count`)).json()) as {
        n: number;
      };
      const b = (await (await fetch(`${router.baseUrl}/count`)).json()) as {
        n: number;
      };
      expect(a.n).toBe(1);
      expect(b.n).toBe(2);
    } finally {
      await router.stop();
    }
  });

  it("honours custom status codes for error responses", async () => {
    const router = await FakeRouter.start({
      routes: [
        {
          method: "POST",
          path: "/v1/chat/completions",
          status: 429,
          body: {
            error: { code: "429", type: "rate_limit_exceeded" },
          },
        },
      ],
    });
    try {
      const res = await fetch(`${router.baseUrl}/v1/chat/completions`, {
        method: "POST",
      });
      expect(res.status).toBe(429);
    } finally {
      await router.stop();
    }
  });
});
