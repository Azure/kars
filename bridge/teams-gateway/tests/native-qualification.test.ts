import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (path: string) =>
  readFileSync(new URL(`../../${path}`, import.meta.url), "utf8");
const workflow = readFileSync(new URL("../../../.github/workflows/bridge-native.yml", import.meta.url), "utf8");
const gate = read("tests/native-credentials/api_gate.py");

describe("Monorepo native prerequisite", () => {
  it("pins core and Bridge to the same source and existing actions without publishing images", () => {
    expect(workflow).toContain("CORE_REVISION: ${{ github.event.pull_request.head.sha || github.sha }}");
    expect(workflow).toContain("path: bridge/.native/core");
    expect(gate).toContain("from source_revision import CORE_REVISION");
    expect(read("tests/native-credentials/source_revision.py")).toContain("expected != revision");
    for (const action of workflow.matchAll(/uses: ([^\n#]+)/g)) {
      expect(action[1].trim()).toMatch(/@[a-f0-9]{40}$/);
    }
    expect(workflow).not.toMatch(/pull_request_target|docker push|freeze-images|secrets\./);
    expect(workflow).toContain("contents: read");
    expect(workflow).toContain("persist-credentials: false");
    expect(workflow).toContain("ref: ${{ github.event.pull_request.head.sha || github.sha }}");
  });

  it("never converts API-only acceptance into runtime, CNI, or active-SRE qualification", () => {
    expect(gate).toContain('"runtimeQualified": False');
    expect(gate).toContain('"networkPolicyEnforcementQualified": False');
    expect(gate).toContain('"activeSreCombinedQualified": False');
    expect(gate).toContain('"karssreregistrations", "--all-namespaces"');
    expect(gate).toContain('"validatingadmissionpolicies"');
    expect(gate).toContain('"expressionWarnings"');
    expect(gate).not.toMatch(/--validate=false|--disable-openapi-validation|failurePolicy.*Ignore/);
    expect(gate).not.toContain("--create-namespace");
    expect(read("tests/native-credentials/api-values.yaml")).toContain("sre:\n  enabled: false");
    expect(gate.indexOf('evidence["schemaPreparation"] = prepare_schemas('))
      .toBeLessThan(gate.indexOf('"helm", "install", "kars"'));
    const boot = read("tests/native-credentials/boot.py");
    expect(boot.indexOf("prepared = prepare_schemas("))
      .toBeLessThan(boot.indexOf('command("helm", "install", "kars"'));
    expect(workflow).toContain("cold_install: [1, 2, 3]");
    expect(workflow).toContain("name: native-api-evidence-${{ matrix.cold_install }}");
  });

  it("parses the runner without executing local Kubernetes or producing cache files", () => {
    for (const file of readdirSync(new URL("../../tests/native-credentials/", import.meta.url))) {
      if (!file.endsWith(".py")) continue;
      execFileSync("python3", ["-c", "import ast,sys; ast.parse(sys.stdin.read())"], {
        input: read(`tests/native-credentials/${file}`),
        stdio: ["pipe", "pipe", "pipe"],
      });
    }
  });

  it("verifies observer diagnostic redaction, source UID fences and network cleanup", () => {
    expect(() => execFileSync("python3", [
      "-m", "unittest", "discover", "-s", "tests/native-credentials",
      "-p", "test_*.py",
    ], {
      cwd: new URL("../../", import.meta.url),
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" },
      stdio: ["ignore", "pipe", "pipe"],
      // Six 20s offline format calls plus ten existing 5s status-shape controls.
      timeout: 180_000,
    })).not.toThrow();
  }, 190_000);

  it("uses real native clients, metadata-only audit, and separate actor TLS contexts", () => {
    const api = read("tests/native-credentials/native_api.py");
    expect(api).toContain('ssl.create_default_context(cadata=self.ca), token');
    expect(api).toContain('"GITHUB_ACTIONS") == "true"');
    expect(api).not.toMatch(/insecure_skip|CERT_NONE|verify=False|kubectl.*proxy/);
    const audit = read("tests/native-credentials/audit-policy.yaml");
    expect(audit).toContain("- level: Metadata");
    expect(audit).not.toMatch(/level: Request/);
    const credentials = read("tests/native-credentials/credential_cases.py");
    expect(credentials).toContain('expected=(403,)');
    expect(credentials).toContain('event["verb"] in MUTATIONS');
    expect(credentials).toContain('event["requestReceivedTimestamp"] > created_at');
    expect(credentials).toContain('"kars.azure.com/credential-grant-uid": "foreign-grant-uid"');
    expect(credentials).toContain('"kars.azure.com/credential-binding-intent": "explicit-reference-v2"');
    expect(credentials).not.toContain("get_metadata");
  });

  it("enrolls writers through the same-source public operator review workflow", () => {
    expect(workflow).toContain("npm ci --prefix .native/core/cli");
    expect(workflow).toContain("npm run build --prefix .native/core/cli");
    const enrollment = read("tests/native-credentials/enrollment.py");
    expect(enrollment).toContain('".native/core/cli/dist/index.js"');
    expect(enrollment).toContain('"credentials", "grant", "preview"');
    expect(enrollment).toContain('"credentials", "grant", "apply"');
    expect(enrollment).toContain('"--private-controller-profile", "service-accounts"');
    expect(enrollment).toContain('setup.ready_grant(namespace)');
    expect(enrollment).not.toContain("admin.create(");
    expect(enrollment).not.toMatch(/failurePolicy|patch.*conditions|break-glass/);
    const continuity = read("tests/native-credentials/grant_continuity_case.py");
    expect(read("tests/native-credentials/run.py"))
      .toContain('case("shared-workspace-and-active-grant-update-continuity"');
    expect(continuity).toContain("previous=reviewed");
    expect(continuity).toContain("other[\"spec\"] == original_spec");
    expect(continuity).toContain("scope_snapshot(setup, scopes) == before_update");
    expect(continuity).toContain('"verb": "use-agent-credentials"');
    expect(continuity).toContain("expected=(403,)");
    const observation = read("tests/native-credentials/observation_cases.py")
      .split("    def enable(self):", 2)[1].split("    def ready(self):", 1)[0];
    expect(observation).toContain("enroll(self.setup, CORE, writer");
    expect(observation).toContain("previous=grant, observations=[");
    expect(observation).not.toContain('self.setup.admin.patch(resource(CORE, "karscredentialgrants"');
  });

  it("qualifies actual CNI traffic and never treats API existence as enforcement", () => {
    expect(workflow).toContain("--version 1.18.5");
    expect(workflow).toContain("needs: [contract-scope, api-admission, native-runtime]");
    expect(workflow).toContain("bash ci/bridge-contract-result.sh");
    const aggregate = readFileSync(new URL("../../../ci/bridge-contract-result.sh", import.meta.url), "utf8");
    expect(aggregate).toContain('${API_RESULT:?Missing API result}');
    expect(aggregate).toContain('${RUNTIME_RESULT:?Missing runtime result}');
    expect(aggregate).toContain("Both native API and runtime acceptance must succeed");
    expect(workflow).not.toContain("continue-on-error:");
    expect(read("tests/native-credentials/kind_config.py")).toContain('"disableDefaultCNI": True');
    const observations = read("tests/native-credentials/observation_cases.py");
    expect(observations).toContain('socket.create_connection(tuple(targets["control"]),3).close()');
    expect(observations).toContain('socket.create_connection((host,port),3)');
    expect(observations).toContain('self.public().get("available") is True');
    expect(observations).toContain('"name": "native-observation-task"');
    expect(observations).not.toContain("self.lifecycle.team_target");
    expect(read("tests/native-credentials/run.py")).toContain('case("team-rebind-uid-namespace-data-and-attestation-continuity", lifecycle.team_rebind)');
    expect(read("tests/native-credentials/run.py")).toContain('["metadataAtFailure"] = diagnostics(setup)');
    expect(observations).toContain('"/egress/learned/clear"');
    expect(read("tests/native-credentials/private_tls.py")).toContain('"x-kars-service-scope"');
  });

  it("requires live CEL behavior, not merely missing type-check warnings", () => {
    const cases = read("tests/native-credentials/admission_cases.py");
    expect(cases).toContain('["data", "stringData"]');
    expect(cases).toContain('"data": ["API_KEY"], "stringData": ["PASSWORD"]');
    expect(cases).toContain('["absent", "null", "non-null"]');
    expect(cases).toContain('"submittedDigest": variant, "storedDigest": wire_kind');
    expect(cases).toContain('"LoadBalancer", "NodePort"');
    expect(cases).toContain('"0.0.0.0/0"');
    expect(cases).toContain('"::/0"');
    expect(cases).toContain('policy in result.get("message", "")');
    expect(cases).toContain('"actor": "setup-admin", "bearerIssuanceQualified": False');
    expect(gate).toContain('evidence["nativeAdmissionCases"]["result"] != "passed"');
  });

  it("preserves exec prohibition and distinguishes ephemeral files from retained namespace data", () => {
    expect(read("tests/native-credentials/Dockerfile.runtime")).toMatch(/^USER 1000:1000$/m);
    expect(read("tests/native-credentials/runtime_state.py")).toContain('"kars-sandbox-exec-ban" in result.stderr');
    const lifecycle = read("tests/native-credentials/lifecycle_cases.py");
    expect(lifecycle).toContain("assert_ephemeral_workspace(pod)");
    expect(lifecycle).toContain("assert_ephemeral_workspace(after_pod)");
    expect(lifecycle).toContain('after_state["dataMarker"] != before_state["dataMarker"]');
    expect(lifecycle).toContain('uid(after_datum) == uid(datum) and after_datum["data"] == datum["data"]');
    expect(lifecycle).toContain('uid(after_pod) != uid(pod)');
    expect(lifecycle).toContain('paused["spec"]["execution"]["launch"] is True');
    expect(read("tests/native-credentials/run.py")).toContain(
      '"workspaceContract": "ephemeral-filesystem-with-namespace-resource-continuity"');
    expect(read("tests/native-credentials/credential_cases.py")).toContain('resource(CORE, "toolpolicies", "kars-default")');
    expect(read("tests/native-credentials/observation_cases.py")).toContain('"toEntities": ["kube-apiserver"]');
    expect(read("tests/native-credentials/observation_cases.py")).not.toContain("break-glass");
    const script = `
import json, os, pathlib, sys, uuid
sys.path.insert(0, "tests/native-credentials")
from runtime_probe import state
from runtime_state import assert_ephemeral_workspace
from native_api import Failure
pod = {"spec":{"containers":[{"name":"openclaw","volumeMounts":[
    {"name":"workspace","mountPath":"/sandbox"}]}],"volumes":[{"name":"workspace","emptyDir":{}}]}}
assert_ephemeral_workspace(pod)
for volume in [
    {"name":"workspace","persistentVolumeClaim":{"claimName":"not-this-contract"}},
    {"name":"workspace","hostPath":{"path":"/tmp"}},
    {"name":"workspace","emptyDir":None},
]:
    invalid = json.loads(json.dumps(pod))
    invalid["spec"]["volumes"] = [volume]
    try:
        assert_ephemeral_workspace(invalid)
    except Failure:
        pass
    else:
        raise AssertionError("non-ephemeral workspace silently accepted")
directory = pathlib.Path(".native") / ("runtime-proof-" + str(uuid.uuid4()))
directory.mkdir(parents=True)
path = directory / "marker"
os.environ["SLACK_BOT_TOKEN"] = "native-secret-must-not-leave-the-process"
try:
    first = state(path)
    retained = state(path)
    assert first["dataWritable"] and not first["dataExistedAtStart"]
    assert retained["dataExistedAtStart"] and retained["dataMarker"] == first["dataMarker"]
    assert first["slackPresent"] is True
    assert "native-secret-must-not-leave-the-process" not in json.dumps(first)
    path.unlink()
    recreated = state(path)
    assert not recreated["dataExistedAtStart"] and recreated["dataMarker"] != first["dataMarker"]
finally:
    path.unlink(missing_ok=True)
    directory.rmdir()
print("runtime-state-checks-passed")
`;
    expect(execFileSync("python3", ["-c", script], {
      cwd: new URL("../../", import.meta.url),
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" },
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim()).toBe("runtime-state-checks-passed");
  });

  it("retains fixed scheduling failure categories without exporting scheduler messages", () => {
    expect(read("tests/native-credentials/run.py")).toContain(
      '"scheduling": scheduling_detail(item) if label == "pods" else None');
    const script = `
import json, sys
sys.path.insert(0, "tests/native-credentials")
from native_api import scheduling_detail
def pod(message, status="False"):
    return {"status":{"conditions":[{"type":"PodScheduled","status":status,"message":message}]}}
result = scheduling_detail(pod("private-node-name: Insufficient cpu; Insufficient memory; node(s) had untolerated taint {private-label}"))
assert result == {"status":"False","categories":["insufficient-cpu","insufficient-memory","untolerated-taint"]}
assert "private-" not in json.dumps(result)
assert scheduling_detail(pod("private-message")) == {"status":"False","categories":["unclassified"]}
assert scheduling_detail(pod("private-message", "True")) == {"status":"True","categories":[]}
assert scheduling_detail({}) == {"status":"Unknown","categories":["unavailable"]}
assert scheduling_detail(pod("node(s) didn't match Pod's node affinity/selector"))["categories"] == ["node-selection"]
assert scheduling_detail(pod("pod has unbound immediate PersistentVolumeClaims"))["categories"] == ["unbound-volume"]
print("scheduling-category-checks-passed")
`;
    expect(execFileSync("python3", ["-c", script], {
      cwd: new URL("../../", import.meta.url),
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" },
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim()).toBe("scheduling-category-checks-passed");
  });

  it("releases only the owned completed Team execution through the normal unlaunch path", () => {
    const runner = read("tests/native-credentials/run.py");
    expect(runner).toContain('case("completed-team-fixture-release", lifecycle.release_team_fixture,');
    expect(runner.indexOf('case("completed-team-fixture-release"')).toBeLessThan(
      runner.indexOf('case("private-bff-observer-and-fresh-privacy-rpc"'));
    expect(runner).toContain('case("private-bff-observer-and-fresh-privacy-rpc", observations.enable)');
    const script = `
import copy, sys
from types import SimpleNamespace
sys.path.insert(0, "tests/native-credentials")
from lifecycle_cases import LifecycleCases
from native_api import Failure, resource
task_path = resource("workspace", "karstasks", "task")
sandbox_path = resource("workspace", "karssandboxes", "sandbox")
namespace_path = "/api/v1/namespaces/kars-sandbox"
class Api:
    def __init__(self):
        self.calls = []
        self.replace_at_write = False
        self.objects = {
            task_path: {"metadata":{"uid":"task-uid","resourceVersion":"7","ownerReferences":[
                {"kind":"KarsTeam","uid":"team-uid","controller":True}]},"spec":{"execution":{"launch":True}}},
            sandbox_path: {"metadata":{"uid":"sandbox-uid"}},
            namespace_path: {"metadata":{"uid":"namespace-uid"}},
        }
    def get(self, path):
        return copy.deepcopy(self.objects[path])
    def optional(self, path):
        return copy.deepcopy(self.objects.get(path))
    def request(self, method, path, body, patch_type=None):
        self.calls.append((path, body))
        assert method == "PATCH" and path == task_path
        assert patch_type == "application/merge-patch+json"
        assert body == {"metadata":{"uid":"task-uid","resourceVersion":"7"},"spec":{"execution":{"launch":False}}}
        if self.replace_at_write:
            self.objects[task_path]["metadata"]["uid"] = "replacement"
            raise Failure("UID conflict")
        self.objects[task_path]["spec"]["execution"]["launch"] = False
        self.objects.pop(sandbox_path)
        self.objects.pop(namespace_path)
        return 200, copy.deepcopy(self.objects[task_path])
for foreign in [None, task_path, sandbox_path, namespace_path, "at-write"]:
    api = Api()
    lifecycle = LifecycleCases(SimpleNamespace(admin=api), None, None)
    lifecycle.team_target = {"workspace":"workspace","task":"task","sandbox":"sandbox",
                            "taskUid":"task-uid","teamUid":"team-uid",
                            "sandboxUid":"sandbox-uid","namespaceUid":"namespace-uid"}
    if foreign == "at-write":
        api.replace_at_write = True
    elif foreign:
        api.objects[foreign]["metadata"]["uid"] = "replacement"
    if foreign:
        try:
            lifecycle.release_team_fixture()
        except Failure:
            pass
        else:
            raise AssertionError("foreign fixture cleanup was allowed")
        assert len(api.calls) == (1 if foreign == "at-write" else 0)
        assert api.objects[task_path]["spec"]["execution"]["launch"] is True
    else:
        lifecycle.release_team_fixture()
        assert len(api.calls) == 1 and task_path in api.objects
print("team-fixture-release-checks-passed")
`;
    expect(execFileSync("python3", ["-c", script], {
      cwd: new URL("../../", import.meta.url),
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" },
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim()).toBe("team-fixture-release-checks-passed");
  });

  it("rejects local native execution and validates signed test principals without disclosure", () => {
    const script = `
import base64, hashlib, hmac, json, os, secrets, sys
sys.path.insert(0, "tests/native-credentials")
import native_api
from boot import principal
os.environ["GITHUB_ACTIONS"] = "false"
try:
    native_api.Setup()
except native_api.Failure:
    pass
else:
    raise AssertionError("local native execution was allowed")
key = secrets.token_hex(32)
token = principal(key)
header, payload, signature = token.split(".")
decode = lambda value: base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
assert json.loads(decode(header))["alg"] == "HS256"
assert json.loads(decode(payload))["roles"] == ["operator", "user"]
assert hmac.compare_digest(decode(signature), hmac.new(
    key.encode(), (header + "." + payload).encode(), hashlib.sha256).digest())
assert native_api.core("workspace", "secrets", "source") == "/api/v1/namespaces/workspace/secrets/source"
detail = native_api.status_detail({
    "reason":"Invalid",
    "message":"private-value-never-log: ValidatingAdmissionPolicy 'kars-boundary' failed: no such key: subResource",
    "details":{"causes":[{"field":"spec.writers","reason":"FieldValueInvalid","message":"another-private-value"}]}})
assert "kars-boundary" in detail and "subResource" in detail and "spec.writers" in detail
assert "private-value" not in detail
print("native-helper-checks-passed")
`;
    const output = execFileSync("python3", ["-c", script], {
      cwd: new URL("../../", import.meta.url),
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" },
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    expect(output.trim()).toBe("native-helper-checks-passed");
  });

  it("uses loaded content digests rather than silently pulling runtime :latest from a registry", () => {
    const script = `
import json, sys
sys.path.insert(0, "tests/native-credentials")
import loaded_images
from native_api import Failure
for name in ["kars-native-runtime", "kars-native-router"]:
    repository = "docker.io/library/" + name
    digest = "a" * 64
    calls = []
    def execute(*args):
        calls.append(args)
        if "inspecti" in args:
            return json.dumps({"status":{"id":"config-identity","repoDigests":[]}})
        if "list" in args:
            ref = json.loads(args[-1].removeprefix("name=="))
            return ("REF TYPE DIGEST SIZE PLATFORMS LABELS\\n" if "@" in ref else
                    f"{ref} application/vnd.oci.image.manifest.v1+json sha256:{digest} 1MiB linux/amd64 -\\n")
        assert "tag" in args
        return ""
    loaded_images.command = execute
    image = loaded_images.loaded_image(name)
    assert image["repository"] + ":" + image["tag"] == repository + "@sha256:" + digest
    assert image["pullPolicy"] == "IfNotPresent"
    assert len([call for call in calls if "tag" in call]) == 2
    assert all("pull" not in call and "push" not in call for call in calls)
try:
    loaded_images.manifest_digest("REF TYPE DIGEST SIZE PLATFORMS LABELS", "missing")
except Failure:
    pass
else:
    raise AssertionError("missing loaded digest silently fell back to a registry")
print("loaded-digest-checks-passed")
`;
    expect(execFileSync("python3", ["-c", script], {
      cwd: new URL("../../", import.meta.url),
      env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" },
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }).trim()).toBe("loaded-digest-checks-passed");
  });
});
