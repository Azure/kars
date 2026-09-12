"""Generate only a disposable Kind topology with enforced CNI and metadata audit."""

import json
from pathlib import Path

root = Path(__file__).resolve().parents[2]
state = root / ".native"
state.mkdir(mode=0o700, exist_ok=True)
(state / "audit").mkdir(mode=0o700, exist_ok=True)
patch = """kind: ClusterConfiguration
apiServer:
  extraArgs:
    audit-policy-file: /etc/kars-native/audit-policy.yaml
    audit-log-path: /var/log/kars-native-audit/audit.log
    audit-log-maxsize: "25"
    audit-log-maxbackup: "1"
  extraVolumes:
    - name: native-audit-policy
      hostPath: /etc/kars-native/audit-policy.yaml
      mountPath: /etc/kars-native/audit-policy.yaml
      readOnly: true
      pathType: File
    - name: native-audit-log
      hostPath: /var/log/kars-native-audit
      mountPath: /var/log/kars-native-audit
      pathType: DirectoryOrCreate
"""
config = {
    "kind": "Cluster", "apiVersion": "kind.x-k8s.io/v1alpha4",
    "networking": {"disableDefaultCNI": True, "podSubnet": "10.244.0.0/16"},
    "nodes": [
        {"role": "control-plane", "kubeadmConfigPatches": [patch], "extraMounts": [
            {"hostPath": str(root / "tests/native-credentials/audit-policy.yaml"),
             "containerPath": "/etc/kars-native/audit-policy.yaml", "readOnly": True},
            {"hostPath": str(state / "audit"), "containerPath": "/var/log/kars-native-audit"},
        ]},
        {"role": "worker", "labels": {"kars.azure.com/pool": "sandbox"}},
    ],
}
(state / "kind.json").write_text(json.dumps(config, indent=2) + "\n")
