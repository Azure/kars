# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Throwaway no-credential network controls, never mutations of the SRE guard."""

import copy
import json
import re

from .common import STANDIN, require


def guard_variant(root, pod, variant):
    require(variant in ("full", "filter-only", "legacy-full"), "Unknown guard comparison")
    source = (root / "controller/src/reconciler/pod_spec.rs").read_text()
    body = source.split("pub(crate) fn build_egress_guard_command", 1)[1].split("\n    cmd\n", 1)[0]
    literals = re.findall(r'cmd\.push_str\(\s*("(?:\\.|[^"\\])*")\s*\)', body)
    require(len(literals) == 8, "Guard comparison requires the exact reviewed command structure")
    expected = "".join(json.loads(literal) for literal in literals)
    require(not any(char in expected for char in ("$", "`", ";", "\n")),
            "Guard comparison refuses dynamic shell constructs")
    matches = [item for item in pod["spec"].get("initContainers", []) if item["name"] == "egress-guard"]
    require(len(matches) == 1, "Guard comparison requires exactly one actual egress guard")
    original = matches[0]
    require(original["image"] == STANDIN and original["command"] == ["sh", "-c", expected]
            and not any(original.get(key) for key in ("env", "envFrom", "volumeMounts")),
            "Actual guard differs from the reviewed source or contains extra inputs")
    script = expected
    if variant == "filter-only":
        script = " && ".join(part for part in script.split(" && ") if not part.startswith("iptables -t nat "))
        require("iptables -A OUTPUT -m owner --uid-owner 1000 -j DROP" in script,
                "Filter comparison must retain the full agent DROP boundary")
    elif variant == "legacy-full":
        script = re.sub(r"\biptables(?= )", "iptables-legacy", script)
        script = "command -v iptables-legacy >/dev/null || exit 42; " + script
    init = {key: copy.deepcopy(value) for key, value in original.items()
            if key in ("name", "image", "imagePullPolicy", "command", "securityContext", "resources")}
    init["command"] = ["sh", "-c", script]
    return init
