---
name: foundry-deployments
description: Query model deployments, connections, and indexes in the Foundry project. Discover available models and infrastructure.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Infrastructure — Deployments, Connections & Indexes

You can query the Foundry project infrastructure to discover available model deployments, data connections, knowledge indexes, and datasets.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### List model deployments

```bash
curl -s 'http://localhost:8443/deployments?api-version=2025-11-15-preview'
```

Returns all deployed models with name, publisher, version, SKU, and capabilities.

### List connections

```bash
curl -s 'http://localhost:8443/connections?api-version=2025-11-15-preview'
```

Returns project connections (Azure AI Search, Bing, storage, etc.) with type and target URL.

### List knowledge indexes

```bash
curl -s 'http://localhost:8443/indexes?api-version=2025-11-15-preview'
```

Returns available search indexes (Azure AI Search, Cosmos DB) for RAG scenarios.

### List datasets

```bash
curl -s 'http://localhost:8443/datasets?api-version=2025-11-15-preview'
```

Returns datasets used for evaluation, fine-tuning, or agent training.

### Get insights

```bash
curl -s 'http://localhost:8443/insights?api-version=2025-11-15-preview'
```

Returns evaluation insights and cluster analysis results.

## When to use

- Discovering which models are available: "what models can I use?"
- Checking infrastructure: "what connections are configured?"
- Finding knowledge bases: "what indexes exist for search?"
- Listing datasets for evaluation or fine-tuning

## When NOT to use

- For running inference (use /v1/chat/completions)
- For creating resources (that requires Azure Portal or Bicep)
