---
name: foundry-evaluations
description: Evaluate agent quality using Foundry OpenAI Evals API. Create evaluations, run them against models, and analyze results.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Evaluations — OpenAI Evals API

You can evaluate agent and model quality using the Foundry OpenAI Evals API. Create evaluation definitions with testing criteria, run them against models, and analyze pass/fail results.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### List evaluations

```bash
curl -s 'http://localhost:8443/openai/evals?api-version=2025-11-15-preview'
```

### Create an evaluation

```bash
curl -s -X POST 'http://localhost:8443/openai/evals?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"name":"quality-check","data_source_config":{"type":"custom","item_schema":{"type":"object","properties":{"input":{"type":"string"},"expected":{"type":"string"}},"required":["input","expected"]}},"testing_criteria":[{"type":"string_check","name":"exact-match","input":"{{sample.output_text}}","reference":"{{item.expected}}","operation":"eq"}]}'
```

### Run an evaluation

```bash
curl -s -X POST 'http://localhost:8443/openai/evals/eval_abc123/runs?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"name":"run-1","data_source":{"type":"jsonl","source":{"type":"file_content","content":[{"item":{"input":"2+2","expected":"4"}}]}}}'
```

### List evaluators (built-in + custom)

```bash
curl -s 'http://localhost:8443/evaluators?api-version=2025-11-15-preview'
```

### List evaluation rules

```bash
curl -s 'http://localhost:8443/evaluationrules?api-version=2025-11-15-preview'
```

## When to use

- Measuring agent quality (accuracy, safety, groundedness)
- A/B testing model versions or prompt changes
- Continuous evaluation of production agent responses
- Red-teaming and safety testing

## When NOT to use

- For runtime inference (use /v1/chat/completions or /openai/responses)
- For conversation management (use foundry-conversations skill)
