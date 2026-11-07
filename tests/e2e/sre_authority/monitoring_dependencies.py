# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Guarded monitoring API prerequisites shared by historical SRE fixtures."""

import json

from .common import CONTEXT, SYSTEM, require


def historical_stage_args(h):
    saved = json.loads(h.run(["helm", "--kube-context", CONTEXT, "get", "values", "kars",
                              "--namespace", SYSTEM, "--all", "-o", "json"]))
    sre = saved.get("sre", {})
    require(sre.get("enabled") is True
            and all(isinstance(sre.get(key), str) and sre[key] for key in ("provider", "model")),
            "Historical SRE must retain its installed provider/model selection")
    return ["upgrade", "kars", str(h.root / "deploy/helm/kars"), "--kube-context", CONTEXT,
            "--namespace", SYSTEM, "--reset-then-reuse-values", "--set", "sre.authorityStage=true",
            "--set-json", "sre.provider=" + json.dumps(sre["provider"]),
            "--set-json", "sre.model=" + json.dumps(sre["model"])]


def prepare_monitoring_dependencies(h, helm_args):
    # Plan static dependency schemas before the authority target needs live
    # SRE lookups and Helm's server resource mapping. Only dependency CRDs write.
    bootstrap = ["install", "kars", str(h.root / "deploy/helm/kars"),
                 "--kube-context", CONTEXT, "--namespace", SYSTEM]
    script = """
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
const require = createRequire(process.cwd() + '/cli/package.json');
const { execa } = await import(require.resolve('execa'));
const library = name => import(pathToFileURL(process.cwd() + '/cli/dist/lib/' + name + '.js').href);
const { renderCoreSchemaChart } = await library('core-helm-schemas');
const { isMonitoringCrd, monitoringSchemaFingerprint, planMonitoringSchemas } = await library('monitoring-schemas');
const invocation = JSON.parse(readFileSync(0, 'utf8'));
const { run, documents } = await renderCoreSchemaChart(execa, invocation.bootstrap);
const apply = await planMonitoringSchemas(run, documents);
await apply();
const target = await renderCoreSchemaChart(execa, invocation.upgrade);
if (monitoringSchemaFingerprint(await target.serverRender()) !== monitoringSchemaFingerprint(documents)) {
  throw new Error('Monitoring dependency changed after its schema plan');
}
console.log(JSON.stringify({ monitoringSchemas: documents.filter(isMonitoringCrd).length, published: true }));
"""
    result = json.loads(h.run(["node", "--input-type=module", "-e", script],
                              data=json.dumps({"bootstrap": bootstrap, "upgrade": helm_args}), timeout=120))
    require(result.get("published") is True and type(result.get("monitoringSchemas")) is int
            and result["monitoringSchemas"] > 0,
            "Historical upgrade must publish its enabled monitoring dependency schemas")
    return result["monitoringSchemas"]
