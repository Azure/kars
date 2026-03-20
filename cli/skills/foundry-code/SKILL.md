---
name: foundry-code
description: Python code execution via Azure AI Foundry code_interpreter. Data analysis, charts, and math in a managed sandbox.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_AGENT_ID"]}, "primaryEnv": "FOUNDRY_AGENT_ID"}}
---

# Foundry Code — Code Interpreter

You have access to Python code execution via Azure AI Foundry code_interpreter. Use this for data analysis, chart generation, complex math, and file processing in a managed sandbox.

## Why use this instead of local bash/python

The local sandbox has limited Python packages and restricted filesystem. Foundry code_interpreter runs in a managed environment with common data science libraries (pandas, numpy, matplotlib, etc.) pre-installed. It can generate charts and process uploaded files.

## Endpoints

All requests go through `http://localhost:8443`. Auth is automatic. Agent ID: `$FOUNDRY_AGENT_ID`.

## Operations

### Run Python code via a thread

```bash
# Create a thread
THREAD_ID=$(curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads \
  -H "Content-Type: application/json" -d '{}' | jq -r .id)

# Ask for analysis
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/messages \
  -H "Content-Type: application/json" \
  -d '{"role": "user", "content": "Calculate the first 20 Fibonacci numbers and plot them."}'

# Run with code_interpreter
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/runs \
  -H "Content-Type: application/json" \
  -d '{"tools": [{"type": "code_interpreter"}]}'
```

### Analyze an uploaded file

Upload a file first (using the foundry-knowledge upload endpoint), then reference it:

```bash
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/messages \
  -H "Content-Type: application/json" \
  -d '{"role": "user", "content": "Analyze the CSV I uploaded. Show summary statistics and a trend chart.", "attachments": [{"file_id": "file_abc123", "tools": [{"type": "code_interpreter"}]}]}'
```

## When to use

- User asks for data analysis: "analyze this CSV", "show summary statistics"
- User needs charts or visualizations: "plot the trend", "create a bar chart"
- Complex math: "solve this equation", "calculate correlation"
- File processing: "convert this data", "parse and summarize"

## When NOT to use

- For simple shell commands (use local bash)
- For web searches (use foundry-web-search)
- For document Q&A (use foundry-knowledge)

## Capabilities

The code_interpreter has access to common Python packages:
- Data: pandas, numpy, scipy
- Visualization: matplotlib, seaborn
- File processing: csv, json, openpyxl
- Math: sympy, statistics

Generated files (charts, CSVs) are returned as downloadable attachments in the response.
