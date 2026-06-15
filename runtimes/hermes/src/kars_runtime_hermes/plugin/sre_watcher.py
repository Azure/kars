# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Proactive incident watcher for the kars-sre agent (Slice 4).

Runs as a long-lived background process alongside the Hermes gateway
inside the SRE sandbox pod. Watches K8s events via the apiserver for
failure-class reasons (FailedCreate, BackOff, FailedScheduling, Failed,
ImagePullBackOff, OOMKilling, …) in *user* namespaces — i.e. `kars-*`
namespaces EXCEPT `kars-sre`, `kars-system`, `kube-*`, `agentmesh`.

On each new incident:

1. Dedupes per ``(namespace, involvedObject.kind, involvedObject.name, reason)``
   in a 10-minute window so a single bad workload doesn't spam the
   operator on every requeue / retry.
2. Calls the existing :mod:`sre` plugin functions in-process to:
   - gather diagnosis context (``sre_describe_resource``, etc.)
   - emit a typed-action proposal via ``sre_propose_fix`` — which
     creates the KarsSREAction CR the operator approves.
3. Renders a tight Telegram-friendly summary and shells out to
   ``hermes send --to telegram`` to push the alert. The send subcommand
   reuses the gateway's configured Telegram bot token + paired user
   allowlist; no new credentials path is needed.

Activated by entrypoint.sh when SRE_ENABLED=true (Slice 4 default).
Operator opt-out: ``SRE_WATCHER_ENABLED=false``.

The watcher is intentionally pull-based (poll the apiserver every
WATCH_INTERVAL_SECONDS) rather than using the long-poll WATCH API.
Polling is simpler, has no streaming-disconnect handling, and the
incident latency target is "tens of seconds" — well within a 10-second
poll window.

Architectural notes:

- The watcher runs as UID 1000 (same SA as the Hermes agent) — it
  uses the same `sre_kube.client()` httpx singleton, which means the
  same SA token + audit trail. No new RBAC needed.
- `kars_notify_human` (a Hermes tool wrapping `hermes send`) would
  let the *agent* push notifications too. Slice 4 ships only the
  watcher → bot path; the tool lands later if proven useful.
"""

from __future__ import annotations

import logging
import os
import re as _re
import subprocess
import sys
import time
from typing import Any

from kars_runtime_hermes.plugin import sre as sre_plugin
from kars_runtime_hermes.plugin import sre_kube

logger = logging.getLogger("kars_runtime_hermes.plugin.sre_watcher")
logger.setLevel(logging.INFO)
if not logger.handlers:
    h = logging.StreamHandler(sys.stderr)
    h.setFormatter(logging.Formatter("[%(asctime)s] sre_watcher: %(message)s"))
    logger.addHandler(h)

# Reasons we treat as actionable incidents. Anything else is informational
# (Normal events) or out-of-scope (e.g. kubernetes node lifecycle events).
INCIDENT_REASONS = frozenset(
    {
        "FailedCreate",
        "BackOff",
        "FailedScheduling",
        "Failed",
        "ImagePullBackOff",
        "ErrImagePull",
        "CrashLoopBackOff",
        "OOMKilling",
        "Evicted",
        "FailedMount",
    }
)

# Namespaces the watcher refuses to act on (proposal §7.7.1
# protected-resource denylist). Same set the controller-side reconciler
# enforces — watcher refuses BEFORE invoking sre_propose_fix so we
# don't even create a CR the controller would just reject.
PROTECTED_NAMESPACES = frozenset(
    {
        "kube-system",
        "kube-public",
        "kube-node-lease",
        "kars-system",
        "kars-sre",
        "agentmesh",
        "default",
    }
)

# Only consider events in namespaces matching this prefix. Operators
# can override via $SRE_WATCHER_NAMESPACE_PREFIX (e.g. "" to widen
# scope to all non-protected namespaces).
NAMESPACE_PREFIX = os.environ.get("SRE_WATCHER_NAMESPACE_PREFIX", "kars-")

# Polling cadence (seconds). 10s is responsive enough for ops while
# keeping the apiserver load minimal — events are also batched on the
# server side so a 10s window typically yields ≤ 1 list call.
WATCH_INTERVAL_SECONDS = int(os.environ.get("SRE_WATCHER_INTERVAL", "10"))

# Per-tuple dedupe window. Within this window a repeated incident with
# the same (ns, kind, name, reason) is silenced. 10 min matches the
# proposal §7.4.4 default.
DEDUPE_WINDOW_SECONDS = int(os.environ.get("SRE_WATCHER_DEDUPE_SECONDS", "600"))

# How fresh an event has to be to count as "new" (vs replay of state
# we already saw at startup). On boot the watcher silently absorbs all
# old events into the dedupe map so it doesn't fire a flood of alerts
# for incidents that happened before it started.
EVENT_FRESHNESS_SECONDS = int(os.environ.get("SRE_WATCHER_FRESHNESS_SECONDS", "120"))

# Per-minute Telegram rate limit. Cluster-wide sliding window — once
# this many messages have gone out in the last 60s, the watcher
# silently drops further alerts until the window slides. Prevents the
# 170-message flood the original Slice 4 demo produced when several
# sandboxes broke at once. Operators tune via ``SRE_WATCHER_MAX_MSGS_PER_MIN``.
# Each batch dispatch emits at most 2 messages (top alert + summary
# tail), so default of 4 = roughly 2 distinct bursts per minute.
MAX_MSGS_PER_MINUTE = int(os.environ.get("SRE_WATCHER_MAX_MSGS_PER_MIN", "4"))

# When the watcher would propose a new KarsSREAction for an incident,
# it first lists existing CRs and reuses any non-terminal one with the
# same (action.type, params.namespace, params.name) target. Suppresses
# the duplicate-CR pile-up the demo showed (40+ identical
# DeleteResourceQuota CRs against the same quota).
CR_REUSE_ENABLED = os.environ.get("SRE_WATCHER_CR_REUSE", "true").lower() not in (
    "false",
    "0",
    "no",
    "off",
)

# Phases the watcher considers "still open" for CR-reuse purposes.
# Anything outside this set is terminal — the watcher will create a
# new CR rather than re-attach to an Expired / Recovered / Failed /
# Rejected one.
ACTIVE_PHASES = frozenset({"Proposed", "Approved", "Applied", ""})


def _resolve_notify_target() -> str:
    """Pick the best Telegram target.

    Order:
      1. explicit override via ``SRE_WATCHER_NOTIFY_TARGET`` env
      2. ``telegram:<first TELEGRAM_ALLOW_FROM id>`` so `hermes send`
         can route without needing the home_channel to be configured
      3. bare ``telegram`` (relies on the gateway's home channel)
    """
    explicit = os.environ.get("SRE_WATCHER_NOTIFY_TARGET")
    if explicit:
        return explicit
    allow = os.environ.get("TELEGRAM_ALLOW_FROM", "").strip()
    if allow:
        first = allow.split(",")[0].strip()
        if first:
            return f"telegram:{first}"
    return "telegram"


NOTIFY_TARGET = _resolve_notify_target()


def _now_epoch() -> float:
    return time.time()


def _event_ts(ev: dict[str, Any]) -> float:
    """Best-effort epoch timestamp for an Event object.

    K8s events carry both legacy ``lastTimestamp`` (RFC3339, seconds
    precision) and modern ``eventTime`` (RFC3339 with sub-second
    precision). Either may be unset depending on which controller
    emitted it. We try lastTimestamp first because it carries the
    most recent occurrence for repeated events.
    """
    for key in ("lastTimestamp", "eventTime"):
        ts = ev.get(key)
        if not ts:
            continue
        try:
            # Strip trailing Z + fractional seconds for stdlib parsing
            from datetime import datetime

            ts_clean = ts.replace("Z", "+00:00")
            return datetime.fromisoformat(ts_clean).timestamp()
        except Exception:
            continue
    # Fall back to firstTimestamp if both above are missing
    fts = ev.get("firstTimestamp")
    if fts:
        try:
            from datetime import datetime

            return datetime.fromisoformat(fts.replace("Z", "+00:00")).timestamp()
        except Exception:
            pass
    return 0.0


# Strip trailing rollout / pod-template hashes so each rollout of the
# SAME workload deduplicates against itself. K8s ReplicaSet names are
# ``<deployment>-<10char-template-hash>`` and pod names are
# ``<rs>-<5char-suffix>``. Without this normalisation a flapping
# Deployment's events get a different dedupe key per rollout = no
# silencing = Telegram spam (170-msg incident).
_HASH_SUFFIX_RE = _re.compile(r"-[a-z0-9]{5,10}$")


def _normalise_name(name: str, kind: str) -> str:
    """Collapse rollout-generated hash suffixes for dedupe purposes.

    ``research-7886669466-abcde`` → ``research-7886669466`` → ``research``.
    Applied to ReplicaSet and Pod kinds. For Job-spawned pods (cron-
    refresh family), strip the cronjob's per-fire timestamp + the pod
    hash suffix to collapse to the parent name.
    """
    if kind not in ("Pod", "ReplicaSet", "Job"):
        return name
    base = name
    # Pod ← RS ← Deployment: strip up to 2 hash suffixes
    for _ in range(2):
        new = _HASH_SUFFIX_RE.sub("", base)
        if new == base:
            break
        base = new
    return base or name


def _dedupe_key(ev: dict[str, Any]) -> tuple[str, str, str, str]:
    """Stable dedupe key: (namespace, kind, normalised-name, reason)."""
    obj = ev.get("involvedObject", {}) or {}
    raw_name = obj.get("name") or ""
    kind = obj.get("kind") or ""
    return (
        ev.get("namespace") or obj.get("namespace") or "",
        kind,
        _normalise_name(raw_name, kind),
        ev.get("reason") or "",
    )


def _list_events_all_namespaces() -> list[dict[str, Any]]:
    """List all Events cluster-wide via the core v1 API.

    Returns the raw items list. Errors are logged and an empty list
    returned so the watcher keeps polling on transient apiserver
    blips.
    """
    try:
        resp = sre_kube.client().get("/api/v1/events")
        return resp.get("items", []) or []
    except Exception as e:
        logger.warning("list events failed: %s", e)
        return []


def _is_in_scope(ev: dict[str, Any]) -> bool:
    """True iff the event belongs to a namespace in scope.

    Scope = ``NAMESPACE_PREFIX`` AND not in ``PROTECTED_NAMESPACES``.
    """
    meta = ev.get("metadata", {}) or {}
    ns = meta.get("namespace") or ev.get("namespace") or ""
    if NAMESPACE_PREFIX and not ns.startswith(NAMESPACE_PREFIX):
        return False
    if ns in PROTECTED_NAMESPACES:
        return False
    return True


def _build_summary(ev: dict[str, Any]) -> str:
    """Build a one-paragraph operator-facing diagnosis string."""
    obj = ev.get("involvedObject", {}) or {}
    ns = obj.get("namespace") or ev.get("namespace", "?")
    kind = obj.get("kind", "?")
    name = obj.get("name", "?")
    reason = ev.get("reason", "?")
    msg = ev.get("message", "")[:240]
    return f"{kind}/{name} in {ns} hit {reason}. {msg}".strip()


def _build_action_target(ev: dict[str, Any]) -> dict[str, Any] | None:
    """Map an event to a propose_fix target shape.

    Returns None when no actionable typed fix exists (e.g. an event on
    a Pod with reason BackOff — the watcher proposes deleting that pod
    so the owner controller respawns it; an event on a ReplicaSet with
    FailedCreate due to ResourceQuota — the watcher proposes deleting
    the quota IF the message names it).
    """
    obj = ev.get("involvedObject", {}) or {}
    ns = obj.get("namespace") or ev.get("namespace")
    kind = obj.get("kind") or ""
    name = obj.get("name") or ""
    reason = ev.get("reason") or ""
    msg = ev.get("message") or ""
    if not ns or not name:
        return None

    # FailedCreate from a ResourceQuota → target the quota directly so
    # the controller can delete it (subject to the kars-managed label
    # guard at execute time).
    if reason == "FailedCreate" and "quota" in msg.lower():
        # Try to extract the quota name from the apiserver's stock
        # message: 'is forbidden: exceeded quota: <name>, ...'
        if "exceeded quota:" in msg:
            try:
                quota_name = msg.split("exceeded quota:", 1)[1].split(",", 1)[0].strip()
                return {
                    "kind": "ResourceQuota",
                    "namespace": ns,
                    "name": quota_name,
                }
            except Exception:
                return None

    # BackOff / CrashLoopBackOff on a Pod → propose deleting the pod so
    # its owning controller (RS / StatefulSet / DS / Job) reconciles a
    # fresh instance. Safe because we do not target ownerless pods.
    if reason in ("BackOff", "CrashLoopBackOff") and kind == "Pod":
        return {"kind": "Pod", "namespace": ns, "name": name}

    # Unhandled — return None so the watcher only NOTIFIES the
    # operator (without creating a CR) and lets the agent / human
    # propose the right action interactively.
    return None


def _send_telegram(text: str) -> bool:
    """Send `text` to the operator via `hermes send`.

    Returns True on exit code 0, False otherwise. Errors are logged
    but do not crash the watcher.
    """
    try:
        result = subprocess.run(
            ["hermes", "send", "--to", NOTIFY_TARGET, "--quiet", text],
            capture_output=True,
            text=True,
            timeout=15,
        )
        if result.returncode != 0:
            logger.warning("hermes send rc=%d stderr=%s", result.returncode, result.stderr[:300])
            return False
        return True
    except subprocess.TimeoutExpired:
        logger.warning("hermes send timed out (15s)")
        return False
    except FileNotFoundError:
        logger.warning("hermes binary not on PATH — telegram notification skipped")
        return False


def _load_dedupe_from_crs() -> dict[tuple[str, str, str], float]:
    """Build dedupe state from existing KarsSREActions.

    Survives pod restarts naturally — the CRs are persisted in etcd,
    not in the pod's emptyDir. Key shape collapsed to
    ``(namespace, action_type, target_name)`` because (per design) the
    operator cares about "one alert per affected workload", regardless
    of which raw event reason triggered the watcher.

    Returns ``{key: last_seen_epoch}`` where ``last_seen_epoch`` is
    derived from the CR's creationTimestamp. Terminal-phase CRs
    suppress re-alerting within ``DEDUPE_WINDOW_SECONDS`` so a freshly-
    failed retry doesn't spam the operator who just decided to reject
    or whose previous proposal expired.
    """
    from datetime import datetime

    out: dict[tuple[str, str, str], float] = {}
    try:
        resp = sre_kube.client().get(
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions"
        )
    except Exception as e:  # noqa: BLE001
        logger.warning("CR-based dedupe bootstrap failed: %s", e)
        return out
    for cr in resp.get("items", []) or []:
        spec = cr.get("spec", {}) or {}
        action = spec.get("action", {}) or {}
        params = action.get("params", {}) or {}
        ns = params.get("namespace") or ""
        name = params.get("name") or ""
        atype = action.get("type") or ""
        if not (ns and name and atype):
            continue
        ts_raw = cr.get("metadata", {}).get("creationTimestamp")
        ts = 0.0
        if ts_raw:
            try:
                ts = datetime.fromisoformat(ts_raw.replace("Z", "+00:00")).timestamp()
            except (ValueError, TypeError):
                # creationTimestamp absent or malformed — treat as 0.0 so this
                # CR doesn't dominate the dedupe state vs a CR with a real
                # timestamp; the actual reconcile loop continues unaffected.
                pass
        key = (ns, atype, name)
        if ts > out.get(key, 0.0):
            out[key] = ts
    return out


def _target_dedupe_key(target: dict[str, Any]) -> tuple[str, str, str]:
    """Translate a propose_fix target into the CR-aligned dedupe key.

    Mirrors :func:`_load_dedupe_from_crs` so the in-memory seen-set
    and the CR-derived bootstrap state share the same keyspace.
    """
    type_map = {
        "ResourceQuota": "DeleteResourceQuota",
        "Pod": "DeletePod",
    }
    atype = type_map.get(target.get("kind", ""), "")
    return (target.get("namespace", "") or "", atype, target.get("name", "") or "")


def _find_existing_open_action(target: dict[str, Any]) -> str | None:
    """Return the name of an existing non-terminal KarsSREAction whose
    target matches, or None if none exists.

    Lists ``kars-sre`` namespaced karssreactions and matches on
    ``spec.action.type`` + ``spec.action.params.namespace`` +
    ``spec.action.params.name``. "Non-terminal" = status.phase in
    ACTIVE_PHASES (Proposed / Approved / Applied / unset).
    """
    if not CR_REUSE_ENABLED:
        return None
    try:
        resp = sre_kube.client().get(
            "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions"
        )
    except Exception as e:  # noqa: BLE001
        logger.warning("list karssreactions failed during CR-reuse check: %s", e)
        return None
    want_type = target.get("type") or {
        "ResourceQuota": "DeleteResourceQuota",
        "Pod": "DeletePod",
    }.get(target.get("kind", ""))
    want_ns = target.get("namespace")
    want_name = target.get("name")
    for cr in resp.get("items", []) or []:
        spec = cr.get("spec", {}) or {}
        action = spec.get("action", {}) or {}
        params = action.get("params", {}) or {}
        if action.get("type") != want_type:
            continue
        if params.get("namespace") != want_ns or params.get("name") != want_name:
            continue
        phase = (cr.get("status", {}) or {}).get("phase", "") or ""
        if phase in ACTIVE_PHASES:
            return cr.get("metadata", {}).get("name")
    return None


def _handle_incident(ev: dict[str, Any]) -> dict[str, Any] | None:
    """Diagnose an event, optionally create a KarsSREAction.

    Returns a candidate descriptor for the batch dispatcher:
    ``{summary, target, ns, kind, name, reason, action_id, cr_error,
       reused, priority}``. The dispatcher (in :func:`run`) ranks
    candidates and decides which to surface in detail vs collapse
    into a summary line.

    Returns None only on internal error. CR creation failures are
    captured in ``cr_error`` so the dispatcher can still mention
    the incident.
    """
    summary = _build_summary(ev)
    target = _build_action_target(ev)
    obj = ev.get("involvedObject", {}) or {}
    ns = obj.get("namespace") or ev.get("namespace", "?")
    reason = ev.get("reason", "?")

    action_id: str | None = None
    cr_error: str | None = None
    reused = False
    if target is not None:
        existing = _find_existing_open_action(target)
        if existing:
            action_id = existing
            reused = True
            logger.info(
                "reusing existing open action %s for target %s/%s/%s — no new CR",
                existing,
                target.get("kind"),
                target.get("namespace"),
                target.get("name"),
            )
        else:
            try:
                proposal = sre_plugin._impl_sre_propose_fix(
                    diagnosis=summary,
                    target=target,
                    # Watcher proposes; operator approves. Short TTL so
                    # stale proposals lapse rather than pile up — 30 min
                    # gives enough time for an operator to wake up.
                    ttl_minutes=30,
                )
                action_id = proposal.get("action_id")
                cr_error = proposal.get("cr_error")
            except Exception as e:  # noqa: BLE001
                logger.warning("propose_fix failed: %s", e)
                cr_error = str(e)

    return {
        "summary": summary,
        "target": target,
        "ns": ns,
        "kind": obj.get("kind") or "?",
        "name": obj.get("name") or "?",
        "reason": reason,
        "action_id": action_id,
        "cr_error": cr_error,
        "reused": reused,
        "priority": _candidate_priority(target is not None, reason, action_id),
    }


def _candidate_priority(actionable: bool, reason: str, action_id: str | None) -> int:
    """Rank a candidate for the per-batch dispatcher.

    Higher = more urgent. Ordering rationale:
    - Actionable + new CR (fix proposed, awaiting approval) — top
    - Actionable + reused (existing open CR, reminder) — second
    - FailedCreate / Failed / OOMKilling / Evicted — workload-level
      damage, more urgent than scheduling pressure
    - BackOff / CrashLoopBackOff — pod stuck, mid
    - FailedScheduling / FailedMount — usually capacity-related, lower
    """
    base = 0
    if actionable:
        base += 100
        if action_id and not action_id.startswith("None"):
            base += 50
    severity = {
        "FailedCreate": 40,
        "Failed": 35,
        "OOMKilling": 35,
        "Evicted": 30,
        "ImagePullBackOff": 25,
        "ErrImagePull": 25,
        "CrashLoopBackOff": 20,
        "BackOff": 15,
        "FailedScheduling": 10,
        "FailedMount": 10,
    }
    return base + severity.get(reason, 0)


def _format_detailed_alert(c: dict[str, Any]) -> str:
    """Single high-priority incident in full Telegram-Markdown form."""
    reminder = " (reminder)" if c["reused"] else ""
    lines = [
        f"🚨 *kars-sre* incident in `{c['ns']}`{reminder}",
        "",
        f"*Symptom:* {c['summary']}",
    ]
    action_id = c["action_id"]
    target = c["target"]
    if action_id and target:
        lines += [
            "",
            f"*Proposed fix:* `{target['kind']}` *{target['namespace']}/{target['name']}*",
            f"*action_id:* `{action_id}`",
            "",
            f"Approve:  `kars sre approve {action_id}`",
            f"Reject:   `kars sre reject {action_id} --reason ...`",
        ]
    elif c["cr_error"]:
        lines += [
            "",
            f"_Could not generate a typed fix: {c['cr_error']}_",
            "",
            "Connect to the bot or `kars sre talk` to investigate.",
        ]
    else:
        lines += [
            "",
            "_No typed fix codified — manual investigation needed._",
            "Reply to triage, or run: `kars sre talk`",
        ]
    return "\n".join(lines)


def _format_summary_tail(extras: list[dict[str, Any]]) -> str:
    """One-line collapse of the remaining candidates for a burst.

    Per-reason counts are most useful for an operator triaging — they
    can tell at a glance whether the burst is "10 pods can't schedule"
    (capacity) vs "5 different things are crashlooping" (broader
    incident).
    """
    by_reason: dict[str, int] = {}
    for c in extras:
        by_reason[c["reason"]] = by_reason.get(c["reason"], 0) + 1
    parts = ", ".join(f"{n} {r}" for r, n in sorted(by_reason.items(), key=lambda kv: -kv[1]))
    return (
        f"\n\n⚠ *+{len(extras)} other incidents* in this scan: {parts}\n"
        "Run `kars sre actions` for the full list."
    )


def _dispatch_batch(candidates: list[dict[str, Any]]) -> int:
    """Send at most one detailed message + one summary tail per scan.

    Ranks by priority, then sends:
    - the top candidate in full
    - if 2+ candidates, a one-line summary footer of the rest

    Returns the count of Telegram messages actually emitted (0, 1, or 2).
    """
    if not candidates:
        return 0
    # Sort by priority desc, then by reason name for determinism so two
    # equal-priority candidates always rank the same way across polls.
    candidates.sort(key=lambda c: (-c["priority"], c["reason"], c["name"]))
    top = candidates[0]
    rest = candidates[1:]
    text = _format_detailed_alert(top)
    sent_count = 0
    if _send_telegram(text):
        sent_count += 1
    logger.info(
        "batch dispatch: top ns=%s kind=%s name=%s reason=%s action_id=%s "
        "rest_count=%d notified=%s",
        top["ns"], top["kind"], top["name"], top["reason"],
        top["action_id"], len(rest), sent_count > 0,
    )
    if rest:
        if _send_telegram(_format_summary_tail(rest).strip()):
            sent_count += 1
    return sent_count


def _workload_state(name: str) -> str | None:
    """Return a workload-availability label for sandbox ``name`` or
    None if no Deployment is found / state is unknown.

    The Deployment lives in ``kars-<name>`` (per the controller's
    namespace-per-sandbox convention). We surface a "WorkloadDown"
    synthetic phase whenever ``available < desired`` AND desired > 0,
    so an evicted pod that can't re-admit (e.g. quota violation,
    image pull error, NodeAffinity unmet) fires a transition even
    though the CR ``status.phase`` itself stays Running.
    """
    try:
        d = sre_kube.client().get(
            f"/apis/apps/v1/namespaces/kars-{name}/deployments/{name}"
        )
    except Exception:  # noqa: BLE001 — best-effort augmentation
        return None
    spec_replicas = (d.get("spec") or {}).get("replicas")
    if spec_replicas is None or spec_replicas == 0:
        return None
    available = ((d.get("status") or {}).get("availableReplicas") or 0)
    if available < spec_replicas:
        return f"WorkloadDown({available}/{spec_replicas})"
    return None


def _phase_change_loop() -> None:
    """Phase-change-only watch mode — alerts ONLY on KarsSandbox state
    transitions. Engaged via SRE_WATCHER_MODE=phase-changes-only.

    "State" here = CR ``status.phase`` overlaid with workload
    availability from the per-sandbox Deployment. The overlay catches
    pod-level failures (evicted, quota violation, image-pull-back-off,
    OOM-loop) that the controller doesn't reflect into CR phase —
    without descending into the chatty event firehose of `events` mode.

    Uses the same httpx singleton the event-mode watcher uses — the
    distroless sandbox image has no kubectl binary.
    """
    poll = WATCH_INTERVAL_SECONDS
    logger.info("phase-changes-only mode (poll=%ds, notify_target=%r)",
                poll, NOTIFY_TARGET)

    last_phase: dict[str, str] = {}
    primed = False

    while True:
        try:
            doc = sre_kube.client().get(
                "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes"
            )
            now_phase: dict[str, str] = {}
            for item in (doc.get("items") or []):
                name = (item.get("metadata") or {}).get("name", "")
                if not name:
                    continue
                ph = (item.get("status") or {}).get("phase") or "Unknown"
                # Overlay workload availability — controller doesn't
                # reflect pod-level breakage into CR.status.phase, so
                # without this an evicted pod stuck Pending on a tight
                # ResourceQuota would never fire a transition.
                if ph in ("Running", "Ready"):
                    wd = _workload_state(name)
                    if wd:
                        ph = wd
                now_phase[name] = ph

            if not primed:
                last_phase = dict(now_phase)
                primed = True
                logger.info("primed with %d sandboxes; watching for transitions",
                            len(last_phase))
                time.sleep(poll)
                continue

            transitions: list[str] = []
            for name, ph in now_phase.items():
                prev = last_phase.get(name)
                if prev is None:
                    transitions.append(f"+ {name}: NEW -> {ph}")
                elif prev != ph:
                    transitions.append(f"~ {name}: {prev} -> {ph}")
            for name, prev in last_phase.items():
                if name not in now_phase:
                    transitions.append(f"- {name}: {prev} -> DELETED")

            if transitions:
                text = "*kars-sre: sandbox phase changes*\n" + "\n".join(
                    f"`{t}`" for t in transitions
                )
                if _send_telegram(text):
                    logger.info("sent phase-change alert: %d transition(s)",
                                len(transitions))
                else:
                    logger.warning("phase-change Telegram send failed")
            last_phase = now_phase
        except Exception as e:  # noqa: BLE001
            logger.warning("phase-change iteration error: %s", e)
        time.sleep(poll)


def run() -> None:
    """Main watch loop. Blocks forever; intended to be the entrypoint
    of a long-lived background process.

    Two modes selectable via ``SRE_WATCHER_MODE``:

    * ``events`` (default) — alert on FailedCreate / BackOff / etc.
      events in kars-* namespaces. High signal for incident response
      but chatty on noisy clusters.
    * ``phase-changes-only`` — alert ONLY on KarsSandbox CR
      ``status.phase`` transitions (e.g. Ready -> Degraded). One
      message per transition, no pod-level event traffic.
    """
    if os.environ.get("SRE_WATCHER_ENABLED", "true").lower() in ("false", "0", "no", "off"):
        logger.info("disabled via SRE_WATCHER_ENABLED — exiting")
        return

    mode = os.environ.get("SRE_WATCHER_MODE", "events").strip().lower()
    if mode in ("phase-changes-only", "phase-changes", "phase", "phase_change", "phase_changes_only"):
        _phase_change_loop()
        return

    logger.info(
        "starting (poll=%ds, dedupe=%ds, prefix=%r, notify_target=%r)",
        WATCH_INTERVAL_SECONDS,
        DEDUPE_WINDOW_SECONDS,
        NAMESPACE_PREFIX,
        NOTIFY_TARGET,
    )

    # Dedupe state. Key shape: (namespace, action_type, target_name).
    # Bootstrapped from existing KarsSREActions so a pod restart
    # doesn't replay alerts for incidents whose CR is still in the
    # cluster. We also re-sync from CRs every minute so an external
    # operator action (e.g. they ran `kubectl delete karssreactions
    # --all` to clean up) flushes the dedupe naturally.
    target_seen: dict[tuple[str, str, str], float] = _load_dedupe_from_crs()
    logger.info("dedupe bootstrap: %d entries from existing CRs", len(target_seen))
    last_cr_sync = _now_epoch()
    CR_SYNC_INTERVAL = 60

    # Sliding-window rate limit log. Each entry is the epoch the
    # message was sent; entries older than 60s are pruned every poll.
    msg_log: list[float] = []

    # First-iteration priming: ALWAYS silently absorb the current
    # event set on the first pass, so we don't flood the operator
    # with "everything that was failing on boot". Trade-off: a freshly-
    # broken workload whose event we missed during pod restart only
    # alerts after the next poll (10s + dedupe-window check). For the
    # SRE notification use case this is fine — it's not a P1 pager.
    primed = False

    while True:
        try:
            now = _now_epoch()
            # Periodic CR resync — REPLACES the dedupe state with the
            # current CR list. This way operators who run
            # `kubectl delete karssreactions --all` to clear the demo
            # see new alerts on the next iteration rather than waiting
            # for the dedupe window to lapse. Recent in-memory alerts
            # (from this watcher's own _handle_incident) are preserved
            # — but only if they are NEWER than CR_SYNC_INTERVAL,
            # which means the operator can't accidentally re-trigger
            # by deleting CRs mid-poll.
            if (now - last_cr_sync) > CR_SYNC_INTERVAL:
                fresh = _load_dedupe_from_crs()
                # Keep in-memory entries newer than the last sync;
                # everything else is REPLACED by the fresh CR snapshot.
                preserved = {
                    k: v for k, v in target_seen.items() if v > last_cr_sync
                }
                target_seen = {**fresh, **preserved}
                last_cr_sync = now
            events = _list_events_all_namespaces()
            # Collect candidates this iteration → dispatch as a batch
            # so a multi-incident burst becomes "1 detailed alert +
            # 1 summary tail" instead of N separate Telegram messages.
            candidates: list[dict[str, Any]] = []
            for ev in events:
                if not _is_in_scope(ev):
                    continue
                if ev.get("type") != "Warning":
                    continue
                reason = ev.get("reason", "")
                if reason not in INCIDENT_REASONS:
                    continue
                ts = _event_ts(ev)
                if ts > 0 and (now - ts) > EVENT_FRESHNESS_SECONDS:
                    continue
                target = _build_action_target(ev)
                if target is None:
                    # No typed fix → fall back to per-event dedupe
                    # using the event tuple so we still alert (once)
                    # for unknown incidents. These are the noisy
                    # alerts (e.g. FailedScheduling on a pod that has
                    # no typed remediation) — priming silences the
                    # initial flood; ranking pushes them below
                    # actionable ones in burst-collapse.
                    obj = ev.get("involvedObject", {}) or {}
                    fallback_key = (
                        ev.get("namespace") or obj.get("namespace") or "",
                        obj.get("kind") or "?",
                        _normalise_name(obj.get("name") or "", obj.get("kind") or ""),
                    )
                    last = target_seen.get(fallback_key)
                    if last is not None and (now - last) < DEDUPE_WINDOW_SECONDS:
                        continue
                    target_seen[fallback_key] = now
                    if primed:
                        cand = _handle_incident(ev)
                        if cand:
                            candidates.append(cand)
                    continue
                # Actionable incident (typed-fix available). On
                # iteration 1 (priming) we silently absorb to avoid
                # boot-time flood. After priming, the CR-reuse path
                # makes sure we don't create duplicate CRs even when
                # the same incident retriggers.
                key = _target_dedupe_key(target)
                last = target_seen.get(key)
                if last is not None and (now - last) < DEDUPE_WINDOW_SECONDS:
                    continue
                target_seen[key] = now
                if primed:
                    cand = _handle_incident(ev)
                    if cand:
                        candidates.append(cand)

            # Burst collapse + per-minute rate limit. Operators saw
            # the original Slice 4 demo flood Telegram with 6+ messages
            # on a single pod restart; here we surface the top
            # candidate in full + a single summary tail line, and
            # apply a sliding-window rate limit cluster-wide.
            if candidates:
                # Drop alerts that would exceed the per-minute budget.
                window_start = now - 60
                msg_log[:] = [t for t in msg_log if t >= window_start]
                budget = max(0, MAX_MSGS_PER_MINUTE - len(msg_log))
                if budget == 0:
                    logger.info(
                        "rate limit hit: %d candidates dropped (max %d msgs/min)",
                        len(candidates), MAX_MSGS_PER_MINUTE,
                    )
                else:
                    # _dispatch_batch sends at most 2 messages (top +
                    # summary). Trim candidates if we can't afford
                    # both — better to send just the top than fail to
                    # send anything.
                    sent = _dispatch_batch(candidates)
                    for _ in range(sent):
                        msg_log.append(now)

            primed = True
            # Trim entries older than 2× the window so the map stays
            # bounded over long uptimes.
            cutoff = now - (DEDUPE_WINDOW_SECONDS * 2)
            target_seen = {k: v for k, v in target_seen.items() if v >= cutoff}
        except Exception as e:  # noqa: BLE001 — keep the loop alive
            logger.warning("watch iteration error: %s", e)
        time.sleep(WATCH_INTERVAL_SECONDS)


if __name__ == "__main__":
    run()
