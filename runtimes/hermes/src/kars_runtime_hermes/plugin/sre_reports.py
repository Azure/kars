# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Report-only publication through the existing registered private SRE facade."""

from __future__ import annotations

import os
import re
from typing import Any

import httpx

from . import sre, sre_kube
from .governance import redact_text

REPORT_PATH = "/reports/latest"
MAX_TEXT_BYTES = 8192
_ANSI = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))")
_ADMISSION_FAILURE = re.compile(
    r"Governed inference budget /v1/reserve unavailable/denied "
    r"\(HTTP (?:Some\([0-9]{3}\)|None)\); no provider fallback"
)
_CALENDAR_FAILURE = re.compile(
    r"(?:Daily|Monthly) token budget exceeded \([0-9]+/[0-9]+ tokens\)\. "
    r"Resets (?:at the next UTC midnight|on the 1st of next month UTC)\."
)


def payload(record: dict[str, Any], interval: int, timeout: int) -> dict[str, Any]:
    text = "\n".join(str(record[key]) for key in ("error", "output") if record.get(key))
    text = _ANSI.sub("", text)
    text = "".join(char for char in text if ord(char) >= 32 or char in "\n\t")
    admission = _ADMISSION_FAILURE.search(text)
    if record["turn_status"] != "finished" and admission:
        # Native Hermes describes every 429 as provider throttling, even local admission failures.
        # The original turn log remains local; do not publish its unsupported fallback advice.
        text = (
            "Kars governed inference admission failed; diagnostic coverage is incomplete. "
            "The reservation was denied or unavailable before that request reached the provider. "
            "This does not establish Azure Foundry throttling or a provider outage.\n"
            f"Router reported: {admission.group()}\n"
            "Review the SRE account's remaining and reserved budget, request bounds, and broker readiness. "
            "Verify admission readiness before retrying; provider/model switching is not an admission remedy. "
            "No automatic funding, refund, fallback, or remedy is authorized. "
            "Earlier tool coverage and token usage remain unverified; original turn output is retained locally."
        )
    calendar = _CALENDAR_FAILURE.search(text)
    if record["turn_status"] != "finished" and calendar:
        text = (
            "Kars calendar token limit blocked inference; diagnostic coverage is incomplete. "
            "This router budget denial does not establish Azure Foundry throttling or a provider outage.\n"
            f"Router reported: {calendar.group()}\n"
            "Review the SRE calendar account, its UID binding, recorded usage and current InferencePolicy limit "
            "separately from lifetime funding. An operator-authorized finite daily-limit increase is supported "
            "for an adopted SRE installation; waiting for UTC rollover is not the only remedy for a daily denial. "
            "A daily-limit increase does not resolve a monthly-limit denial; monthly limits retain their seals. "
            "Preserve recorded usage, pending liabilities and funding receipts. "
            "Provider/model switching does not repair this limit. "
            "No automatic funding, refund, fallback, or remedy is authorized. "
            "Earlier tool coverage and token usage remain unverified; original turn output is retained locally."
        )
    text = redact_text(text).encode("utf-8", errors="replace")
    truncated = bool(record.get("output_truncated")) or len(text) > MAX_TEXT_BYTES
    if len(text) > MAX_TEXT_BYTES:
        # Keep failure context and the final diagnosis/remedies rather than only CLI startup logs.
        marker = b"\n[report excerpt truncated; diagnostic coverage incomplete]\n"
        text = (
            text[:2048].decode("utf-8", errors="ignore").encode()
            + marker
            + text[-(MAX_TEXT_BYTES - 2048 - len(marker)) :].decode("utf-8", errors="ignore").encode()
        )
    return {
        "sweep_id": record["sweep_id"],
        "started_at_ms": int(record["started_at"] * 1000),
        "finished_at_ms": int(record["finished_at"] * 1000),
        "turn_status": record["turn_status"],
        "text": text.decode("utf-8") or "No diagnostic output captured; cluster health is unverified.",
        "truncated": truncated,
        "interval_seconds": interval,
        "timeout_seconds": timeout,
    }


def publish(record: dict[str, Any], interval: int, timeout: int) -> dict[str, Any]:
    if (
        not sre.is_enabled()
        or os.environ.get("KUBERNETES_SERVICE_HOST") != "127.0.0.1"
        or os.environ.get("KUBERNETES_SERVICE_PORT") != "9446"
    ):
        raise RuntimeError("Registered private SRE projection is required for reporting")
    report = payload(record, interval, timeout)
    for attempt in range(2):
        try:
            receipt = sre_kube.client().post(REPORT_PATH, json=report)
            break
        except (httpx.TransportError, httpx.HTTPStatusError) as exc:
            if attempt or (isinstance(exc, httpx.HTTPStatusError) and exc.response.status_code < 500):
                raise
    if (
        not isinstance(receipt, dict)
        or receipt.get("persisted") is not True
        or receipt.get("sweep_id") != report["sweep_id"]
        or receipt.get("enforcement_changed") is not False
        or type(receipt.get("accepted_at_ms")) is not int
        or receipt["accepted_at_ms"] <= 0
        or not re.fullmatch(r"[a-f0-9]{64}", str(receipt.get("digest", "")))
    ):
        raise RuntimeError("Report persistence acknowledgement is invalid")
    return receipt
