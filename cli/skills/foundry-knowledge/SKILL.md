---
name: foundry-knowledge
description: Knowledge retrieval (RAG) via Azure AI Foundry file_search. Search uploaded documents with vector similarity.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_AGENT_ID"]}, "primaryEnv": "FOUNDRY_AGENT_ID"}}
---

# Foundry Knowledge — File Search (RAG)

You have access to knowledge retrieval via Azure AI Foundry file_search. Use this to search uploaded documents, PDFs, and text files with vector similarity — grounding your answers in real data.

## Why use this instead of local grep

Local grep only does text matching. Foundry file_search uses vector embeddings for semantic search — it finds relevant content even when exact keywords don't match. Documents are indexed and searchable across sessions.

## Endpoints

All requests go through `http://localhost:8443`. Auth is automatic (IMDS). Agent ID: `$FOUNDRY_AGENT_ID`.

## Operations

### Upload a file for indexing

```bash
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/files \
  -H "Content-Type: multipart/form-data" \
  -F "file=@/sandbox/document.pdf" \
  -F "purpose=assistants"
```

Returns `{"id": "file_abc123", ...}`.

### Search documents via a run

To search uploaded documents, create a thread and run with `file_search` enabled:

```bash
# Create a thread
THREAD_ID=$(curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads \
  -H "Content-Type: application/json" -d '{}' | jq -r .id)

# Add the question
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/messages \
  -H "Content-Type: application/json" \
  -d '{"role": "user", "content": "What does the report say about Q3 revenue?"}'

# Run with file_search
curl -s -X POST http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/threads/${THREAD_ID}/runs \
  -H "Content-Type: application/json" \
  -d '{"tools": [{"type": "file_search"}]}'
```

The response includes the answer with citations referencing the source documents.

### List uploaded files

```bash
curl -s http://localhost:8443/agents/${FOUNDRY_AGENT_ID}/files
```

## When to use

- User asks questions about uploaded documents: "what does the report say about X"
- User uploads a file and wants to query it
- You need to ground answers in specific source material (RAG)
- User says "search my documents", "find in my files"

## When NOT to use

- For real-time web information (use foundry-web-search)
- For running code or calculations (use foundry-code)
- For simple text files already in /sandbox/ (just read them directly)
