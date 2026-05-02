# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Minimal BYO agent that proxies prompts to the AzureClaw router.

The agent never sees an API key. The router is reachable at
``http://127.0.0.1:8443/openai/v1`` from inside the sandbox; it
authenticates upstream with the workload identity attached to the
sandbox pod and applies the operator's InferencePolicy /
ContentSafety / TokenBudget pipeline before the call goes out.
"""
from __future__ import annotations

import os
from fastapi import FastAPI
from openai import OpenAI
from pydantic import BaseModel

ROUTER_BASE = os.environ.get("AZURECLAW_ROUTER_URL", "http://127.0.0.1:8443") + "/openai/v1"
MODEL = os.environ.get("AZURECLAW_MODEL", "gpt-4.1")

# OpenAI SDK requires *something* in api_key; the router rewrites the
# auth header with an IMDS-issued token before forwarding upstream.
client = OpenAI(base_url=ROUTER_BASE, api_key="azureclaw-router")
app = FastAPI(title="byo-quickstart")


class ChatRequest(BaseModel):
    prompt: str


@app.get("/healthz")
def healthz() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/chat")
def chat(req: ChatRequest) -> dict[str, str]:
    resp = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": req.prompt}],
    )
    return {"reply": resp.choices[0].message.content or ""}
