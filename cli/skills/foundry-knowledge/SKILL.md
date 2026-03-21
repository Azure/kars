---
name: foundry-knowledge
description: Knowledge retrieval (RAG) via Foundry file_search and azure_ai_search tools. Agentic retrieval with citations — uses Responses API.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Knowledge — File Search & Azure AI Search (Responses API)

You have access to knowledge retrieval via Foundry's built-in search tools:
- **file_search**: Searches uploaded files in vector stores (PDFs, docs, text)
- **azure_ai_search**: Searches Azure AI Search indexes (enterprise RAG)

Both return results with citations. No hosted agent needed — uses direct Responses API.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### Search uploaded files (file_search)

Requires vector store IDs from previously uploaded files:

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"What does the Q3 report say about revenue?","tools":[{"type":"file_search","vector_store_ids":["vs_abc123"]}],"store":false}'
```

### Search Azure AI Search index

Requires an AI Search index configured as a connection in the Foundry project:

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"Find documents about security best practices","tools":[{"type":"azure_ai_search","azure_ai_search":{"indexes":[{"index_name":"my-index","project_connection_id":"/connections/my-search"}]}}],"store":false}'
```

### List available indexes

```bash
curl -s 'http://localhost:8443/indexes?api-version=2025-11-15-preview'
```

## When to use

- User asks "what does the report say about X" — RAG over enterprise docs
- Need to ground answers in specific documents with citations
- User says "search my documents", "find in the knowledge base"
- Questions requiring multiple document sources

## When NOT to use

- For real-time web information (use foundry-web-search)
- For code execution or calculations (use foundry-code)
- For simple text files in /sandbox/ (just read them directly)
- For remembering user preferences (use foundry-memory)

## Note on setup

File search requires uploading files to Foundry vector stores first. Azure AI Search requires an index and connection configured in the project. If neither is set up, the tools will return empty results — the agent should fall back to its training data.
