---
name: foundry-code
description: Python code execution via Azure AI Foundry Responses API with code_interpreter tool. Data analysis, charts, and math in a managed sandbox.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Code — Code Interpreter (Responses API)

You have access to Python code execution via the Foundry Responses API `code_interpreter` tool. Use this for data analysis, chart generation, complex math, and file processing. No Foundry hosted agent needed — uses direct Responses API.

## Why use this instead of local bash/python

The local sandbox has restricted packages. Foundry code_interpreter runs server-side with pre-installed data science libraries (pandas, numpy, matplotlib, scipy, etc.) and can generate visualizations.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### Run Python code

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"Calculate the first 20 Fibonacci numbers and show them.","tools":[{"type":"code_interpreter","container":{"type":"auto"}}],"store":false}'
```

The model will write and execute Python code, returning both the code and its output.

### Data analysis

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"Generate 100 random data points, compute mean/median/std, and tell me if normally distributed.","tools":[{"type":"code_interpreter","container":{"type":"auto"}}],"store":false}'
```

## When to use

- User asks for data analysis: "analyze this", "show statistics"
- Charts or visualizations: "plot the trend", "create a chart"
- Complex math: "solve this equation", "calculate correlation"
- Anything requiring Python runtime

## When NOT to use

- For simple shell commands (use local bash)
- For web searches (use foundry-web-search)
- For document Q&A (use foundry-knowledge)
- For remembering preferences (use foundry-memory)
