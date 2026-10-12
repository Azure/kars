# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Preserve default PromQL parsing with Prometheus's instance-based API."""
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
changes = (
    ("apps/dashvalidator/pkg/validator/prometheus/parser.go", "parser.ParseExpr(query)",
     "parser.NewParser(parser.Options{}).ParseExpr(query)"),
    ("pkg/tsdb/grafana-pyroscope-datasource/instance.go", "parser.ParseMetricSelector(labelSelector)",
     "parser.NewParser(parser.Options{}).ParseMetricSelector(labelSelector)"),
)
prepared = []
for relative, before, after in changes:
    path = root / relative
    text = path.read_text()
    if text.count(before) != 1 or after in text:
        raise ValueError(f"Unexpected upstream parser source: {relative}")
    prepared.append((path, text, text.replace(before, after)))
evidence = []
for path, before, after in prepared:
    path.write_text(after)
    evidence.append({"path": str(path.relative_to(root)),
                     "before": hashlib.sha256(before.encode()).hexdigest(),
                     "after": hashlib.sha256(after.encode()).hexdigest()})
print(json.dumps({"promqlParserCompatibility": evidence}))
