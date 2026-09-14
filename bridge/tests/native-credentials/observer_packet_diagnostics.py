# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded, read-only Cilium monitor evidence, never an acceptance oracle.

Wire contract (v1.18.5): pkg/monitor/{datapath_drop,datapath_trace,dissect}.go.
Policy verdicts remain DumpInfo text even with --json: format/format.go.
Neither format provides packet timestamps, PIDs, socket cookies or request IDs.
"""

from datetime import datetime, timezone
import ipaddress
import json
import os
import re
import selectors
import signal
import subprocess
import threading
import time

from api_outcome_diagnostics import project as project_audit, read_audit_tail, rules_for, timestamp
from native_api import ROOT, require
from observation_diagnostics import READ_ERRORS, project as project_router
import observer_network_diagnostics as network

MAX_BYTES = 262144
MAX_LINE = 8192
MAX_EVENTS = 96
MAX_SECONDS = 90
UNAVAILABLE = "Packet diagnostic unavailable"
OBSERVATION_POINTS = frozenset("""
to-endpoint to-proxy to-host to-stack to-overlay to-network to-crypto
from-endpoint from-proxy from-host from-stack from-overlay from-network from-crypto
""".split())
STATES = frozenset("new established reply related reopened unknown srv6-encap srv6-decap encrypt-overlay".split())
# The remote timeout also bounds the process if kubectl or this Python runner dies.
# stdin EOF requests early cleanup; wait reaps the exact child, never a name match.
MONITOR_SCRIPT = """
cilium-dbg monitor --json --numeric --type drop --type trace --type policy-verdict --related-to "$1" 2>/dev/null &
child=$!
trap 'kill "$child" 2>/dev/null; wait "$child" 2>/dev/null' EXIT
trap 'exit 124' TERM INT
printf 'native-monitor-started\\n'
IFS= read -r stop
kill "$child" 2>/dev/null
wait "$child"
status=$?
trap - EXIT
printf '\\nnative-monitor-reaped:%s\\n' "$status"
"""
VERDICT = re.compile(
    r"Policy verdict log: flow 0x[0-9a-f]{1,8} local EP ID ([0-9]{1,5}), "
    r"remote ID ([0-9]{1,10}), proto 6, (egress|ingress), action (allow|deny|redirect|audit), "
    r"auth: (disabled|spire|test-always-fail), match (none|L3-Only|L3-L4|L4-Only|all|L3-Proto|Proto-Only|unknown), "
    r"(\S{1,60}) -> (\S{1,60}) tcp ((?:SYN|ACK|RST|FIN)(?:, (?:SYN|ACK|RST|FIN))*)?"
)
TCP_FLAG = re.compile(r"(?:^|[ {\t])(?P<name>SYN|ACK|RST|FIN)=(?P<value>true|false)(?=[ }\t]|$)")


def uint(value, maximum):
    require(type(value) is int and 0 <= value <= maximum, UNAVAILABLE)
    return value


def address_port(value):
    require(isinstance(value, str) and len(value) <= 60, UNAVAILABLE)
    address, port = value.rsplit(":", 1)
    address = address.removeprefix("[").removesuffix("]")
    require(re.fullmatch(r"[0-9]{1,5}", port) is not None, UNAVAILABLE)
    port = int(port)
    require(0 < port <= 65535, UNAVAILABLE)
    return str(ipaddress.ip_address(address)), port


def tcp_flags(value):
    # LayerString contains more than flags (including options). Never retain it.
    require(isinstance(value, str) and len(value) <= 4096
            and value.startswith("TCP\t"), UNAVAILABLE)
    entries = [(item["name"], item["value"]) for item in TCP_FLAG.finditer(value)]
    require(len(entries) == 4 and {key for key, _ in entries} == {"SYN", "ACK", "RST", "FIN"}, UNAVAILABLE)
    return [key for key in ("SYN", "ACK", "RST", "FIN") if (key, "true") in entries]


def flow_tuple(summary):
    require(isinstance(summary, dict) and summary.get("tunnel") is None
            and not any(summary.get(key) for key in ("udp", "sctp", "icmpv4", "icmpv6")), UNAVAILABLE)
    l3, l4 = summary["l3"], summary["l4"]
    require(isinstance(l3, dict) and isinstance(l4, dict), UNAVAILABLE)
    result = []
    for side in ("src", "dst"):
        address = l3[side]
        port = l4[side]
        require(isinstance(address, str) and len(address) <= 45
                and isinstance(port, str) and re.fullmatch(r"[0-9]{1,5}", port), UNAVAILABLE)
        result.append(address_port(f"[{address}]:{port}"))
    return *result, tcp_flags(summary["tcp"])


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, UNAVAILABLE)
        result[key] = value
    return result


def project_line(raw, binding, destinations):
    """Allowlist real mixed monitor wire records; never echo input strings."""
    require(isinstance(raw, bytes) and len(raw) <= MAX_LINE, UNAVAILABLE)
    try:
        text = raw.decode("utf8")
        if text.startswith("Policy verdict log:"):
            matched = VERDICT.fullmatch(text)
            require(matched is not None, UNAVAILABLE)
            endpoint, remote, direction, action, auth, match, src, dst, flags = matched.groups()
            require(int(endpoint) == binding["endpointId"] and int(remote) < 2**32
                    and direction == "egress", UNAVAILABLE)
            src, dst = address_port(src), address_port(dst)
            flags = flags.split(", ") if flags else []
            require(len(flags) == len(set(flags)), UNAVAILABLE)
            record = {"kind": "policy-verdict", "verdict": action, "remoteIdentity": int(remote),
                      "policyMatch": match, "authentication": auth, "direction": "egress"}
        else:
            event = json.loads(text, object_pairs_hook=unique_object)
            require(isinstance(event, dict) and event.get("type") in ("drop", "trace"), UNAVAILABLE)
            source = uint(event["source"], 65535)
            dst_id = uint(event["dstID"], 2**32 - 1)
            src_label = uint(event["srcLabel"], 2**32 - 1)
            dst_label = uint(event["dstLabel"], 2**32 - 1)
            uint(event["bytes"], 2**32 - 1)
            src, dst, flags = flow_tuple(event["summary"])
            outbound = source == binding["endpointId"] and src_label == binding["securityIdentity"]
            inbound = dst_id == binding["endpointId"] and dst_label == binding["securityIdentity"]
            require(outbound != inbound, UNAVAILABLE)
            record = {"kind": event["type"], "sourceIdentity": src_label, "destinationIdentity": dst_label,
                      "direction": "egress" if outbound else "ingress"}
            if event["type"] == "trace":
                require(event["observationPoint"] in OBSERVATION_POINTS
                        and event["state"] in STATES, UNAVAILABLE)
                record.update(observationPoint=event["observationPoint"], connectionState=event["state"],
                              verdict="trace-not-a-policy-verdict")
            else:
                # Drop reason is an arbitrary string; only one exact upstream reason is named.
                record.update(verdict="drop", reason="policy-denied" if event.get("reason") == "Policy denied"
                              else "other-or-unavailable")
                record["bpfFile"] = uint(event["File"], 255)
                record["bpfLine"] = uint(event["Line"], 65535)
                record["ifindex"] = uint(event["Ifindex"], 2**32 - 1)
        pod_side, api_side = (src, dst) if record["direction"] == "egress" else (dst, src)
        require(pod_side[0] in binding["addresses"] and api_side in destinations, UNAVAILABLE)
        return {**record, "podAddress": pod_side[0], "podPort": pod_side[1],
                "apiAddress": api_side[0], "apiPort": api_side[1], "tcpFlags": flags}
    except READ_ERRORS + (AttributeError, IndexError, RecursionError, UnicodeError):
        return None


def empty_result():
    return {"available": False, "category": "source-unavailable", "events": [],
            "freshRouterRequest": "unavailable", "packetProcessAttribution": "unavailable",
            "freshnessReason": "monitor-has-no-request-id-pid-or-generation-timestamp",
            "timing": "local-receipt-not-packet-generation", "missingEventsAreNotDenials": True,
            "remoteCleanup": "not-started", "localCleanup": "not-started"}


def command_for(before):
    bindings = list(before["stable"]["cilium"]["endpoints"].values())
    require(len(bindings) == len(before["actor"]["pods"]) == 1, UNAVAILABLE)
    binding = bindings[0]
    require(binding["podUid"] == before["actor"]["pods"][0]["uid"], UNAVAILABLE)
    require(0 < uint(binding["endpointId"], 65535), UNAVAILABLE)
    agent = before["facts"]["cilium"]["agent"]
    network.metadata({"metadata": agent})
    return [
        "kubectl", "--context", "kind-bridge-native", "--request-timeout=100s",
        "exec", "-i", "-n", "kube-system", agent["name"], "-c", "cilium-agent", "--",
        "timeout", "--signal=TERM", "--kill-after=2s", "95s",
        "sh", "-c", MONITOR_SCRIPT, "native-observer-monitor", str(binding["endpointId"]),
    ], binding


class Window:
    """Drain a narrow monitor continuously, retaining only bounded projections."""

    def __init__(self, before):
        self.result = empty_result()
        self.started = datetime.now(timezone.utc)
        self.ended = None
        self.result["startedAt"] = self.started.isoformat()
        self.clock = time.monotonic()
        self.stop = threading.Event()
        self.launched = threading.Event()
        self.process = self.thread = None
        self.before = before
        self.closed = False
        try:
            arguments, self.binding = command_for(before)
            self.destinations = {(item["address"], item["port"]) for item in before["facts"]["destinations"]}
            self.process = subprocess.Popen(arguments, cwd=ROOT, stdin=subprocess.PIPE,
                                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                            start_new_session=True)
            self.thread = threading.Thread(target=self._run, name="native-observer-monitor", daemon=True)
            self.thread.start()
            self.launched.wait(3)
        except READ_ERRORS:
            self.close()

    def _run(self):
        process = self.process
        pending = b""
        received = 0
        stopping_at = None
        with selectors.DefaultSelector() as selector:
            try:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    now = time.monotonic()
                    if stopping_at is None and (self.stop.is_set() or now - self.clock >= MAX_SECONDS):
                        stopping_at = now
                        process.stdin.close()
                    if stopping_at is not None and now - stopping_at > 5:
                        break
                    ready = selector.select(0.1)
                    if not ready:
                        if process.poll() is not None:
                            break
                        continue
                    remaining = MAX_BYTES + MAX_LINE - received
                    if remaining <= 0:
                        break
                    chunk = os.read(process.stdout.fileno(), min(4096, remaining))
                    if not chunk:
                        break
                    received += len(chunk)
                    if received > MAX_BYTES:
                        self.result["category"] = "byte-limit"
                        self.stop.set()
                    pending += chunk
                    if len(pending) > MAX_LINE and b"\n" not in pending:
                        self.result["category"] = "line-limit"
                        self.stop.set()
                        pending = b""
                        continue
                    while b"\n" in pending:
                        line, pending = pending.split(b"\n", 1)
                        if line == b"native-monitor-started":
                            self.launched.set()
                            self.result.update(category="unobserved", localCleanup="pending",
                                               remoteCleanup="pending")
                        elif re.fullmatch(rb"native-monitor-reaped:[0-9]{1,3}", line):
                            self.result["remoteCleanup"] = "exact-child-reaped"
                            self.result["monitorExitStatus"] = int(line.rsplit(b":", 1)[1])
                        elif stopping_at is None and not self.stop.is_set() and len(line) <= MAX_LINE:
                            record = project_line(line, self.binding, self.destinations)
                            if record is not None:
                                elapsed = time.monotonic() - self.clock
                                if elapsed >= MAX_SECONDS:
                                    self.stop.set()
                                    continue
                                record["receivedAfterStartMs"] = round(elapsed * 1000)
                                self.result["events"].append(record)
                                self.result.update(available=True, category="positive-packet-evidence")
                                if len(self.result["events"]) >= MAX_EVENTS:
                                    self.result["category"] = "event-limit"
                                    self.stop.set()
                            else:
                                self.result["unmatchedOrInvalidLines"] = self.result.get("unmatchedOrInvalidLines", 0) + 1
            except (OSError, ValueError):
                self.result["category"] = "stream-unavailable"
            finally:
                if pending:
                    self.result["partialLineDiscarded"] = True
                self.result["bytesRead"] = received
                self.ended = datetime.now(timezone.utc)
                self.result["stoppedAt"] = self.ended.isoformat()
                self.result["observedDurationMs"] = round((time.monotonic() - self.clock) * 1000)
                self._reap()

    def _reap(self):
        process = self.process
        if process is None:
            return
        try:
            if process.stdin and not process.stdin.closed:
                process.stdin.close()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=1)
            self.result["localCleanup"] = "reaped"
        except (OSError, subprocess.SubprocessError):
            self.result["localCleanup"] = "unverified"
        finally:
            if process.stdout:
                process.stdout.close()

    def close(self):
        if not self.closed:
            self.closed = True
            self.stop.set()
            if self.thread is not None:
                try:
                    self.thread.join(10)
                except RuntimeError:
                    self._reap()
                if self.thread.is_alive():
                    self.result["localCleanup"] = "unverified"
            else:
                self._reap()
            if self.result["remoteCleanup"] == "pending":
                self.result["remoteCleanup"] = "unverified-remote-timeout-95s-plus-2s"
            if not self.launched.is_set():
                self.result["category"] = "source-unavailable"
            elif (self.result["category"] == "unobserved"
                  and self.result.get("monitorExitStatus") not in (143, 137)):
                self.result["category"] = "source-unavailable"
        return self.result

    def finish(self, setup, target, stable):
        result = self.close()
        result["provenanceUnchanged"] = stable
        if not stable:
            result.update(available=False, category="provenance-changed", events=[])
            return result
        if not hasattr(self, "binding"):
            return result
        result["podUid"] = self.binding["podUid"]
        result["endpointId"] = self.binding["endpointId"]
        result["securityIdentity"] = self.binding["securityIdentity"]
        result["routerContainerProcessUnchanged"] = True
        result["configuredRouterUid"] = 1001
        result["requests"] = request_evidence(setup, target, self.before, self.started,
                                              self.ended or datetime.now(timezone.utc))
        return result


def start(before):
    return Window(before)


def request_evidence(setup, target, before, since, until):
    """Record actual completions and authenticated arrivals, not inferred SYN requests."""
    result = {"routerCompletions": [], "freshPodBoundApiRequests": [],
              "routerLogStatus": "unavailable", "auditStatus": "unavailable",
              "completionDoesNotProveRequestStartedInWindow": True,
              "packetToRequestCorrelation": "unavailable"}
    try:
        require(isinstance(since, datetime) and isinstance(until, datetime)
                and since.tzinfo is not None and until.tzinfo is not None
                and 0 <= (until - since).total_seconds() <= 110, UNAVAILABLE)
    except READ_ERRORS:
        return result
    try:
        pod = before["actor"]["pods"][0]
        raw = subprocess.run(
            ["kubectl", "--context", "kind-bridge-native", "--request-timeout=10s", "logs",
             "-n", before["actor"]["namespace"], pod["name"], "-c", "inference-router",
             "--tail=512", "--limit-bytes=131072", "--since-time=" + since.isoformat()],
            cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=12, check=False)
        require(raw.returncode == 0 and len(raw.stdout) <= 131072, UNAVAILABLE)
        for line in raw.stdout.splitlines():
            if len(line) > MAX_LINE:
                continue
            try:
                value = json.loads(line, object_pairs_hook=unique_object)
                at = timestamp(value.get("timestamp"))
                records = project_router(line.decode("utf8"), "router")
                if at is not None and since <= at <= until and records and records[0]["stage"] == "observer_target_client":
                    result["routerCompletions"].append(
                        {"completedAfterStartMs": round((at - since).total_seconds() * 1000), **records[0]})
            except READ_ERRORS + (AttributeError, RecursionError, UnicodeError):
                continue
        result["routerCompletions"] = result["routerCompletions"][-12:]
        result["routerLogStatus"] = "observed" if result["routerCompletions"] else "unobserved"
    except READ_ERRORS:
        pass
    try:
        actor = before["actor"]
        rules = rules_for(setup, "observer_router", target, actor)
        raw = read_audit_tail()
        for line in raw.splitlines():
            if len(line) > MAX_LINE:
                continue
            try:
                records = project_audit(line, actor, rules, since, until)
                if records:
                    event = json.loads(line, object_pairs_hook=unique_object)
                    received = timestamp(event["requestReceivedTimestamp"])
                    completed = timestamp(event["stageTimestamp"])
                    result["freshPodBoundApiRequests"].append({
                        "receivedAfterStartMs": round((received - since).total_seconds() * 1000),
                        "completedAfterStartMs": round((completed - since).total_seconds() * 1000),
                        "httpStatus": records[0]["http_status"]})
            except READ_ERRORS + (AttributeError, RecursionError):
                continue
        result["freshPodBoundApiRequests"] = result["freshPodBoundApiRequests"][-12:]
        result["auditStatus"] = "observed" if result["freshPodBoundApiRequests"] else "unobserved"
    except READ_ERRORS:
        pass
    return result
