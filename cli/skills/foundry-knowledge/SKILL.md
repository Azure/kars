---
name: foundry-knowledge
description: Knowledge retrieval (RAG) via Foundry IQ and Azure AI Search. Agentic retrieval with citations — no Foundry hosted agent needed.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Knowledge — Foundry IQ (Direct)

You have access to knowledge retrieval via Foundry IQ — a managed knowledge layer backed by Azure AI Search. It provides agentic retrieval: query decomposition, parallel search, semantic reranking, and citation-backed answers. No Foundry hosted agent needed — this uses direct APIs.

## Why use this instead of local grep

Local grep does text matching only. Foundry IQ uses:
- **Vector + hybrid search** — finds relevant content even when exact keywords don't match
- **Agentic retrieval** — decomposes complex questions into sub-queries, runs them in parallel
- **Multi-source** — searches across Azure Blob Storage, SharePoint, OneLake, and web
- **ACL-aware** — respects permissions and Purview sensitivity labels
- **Citations** — returns source references so you can trace answers

## Endpoints

All requests go through `http://localhost:8443`. Auth is automatic (IMDS).

Knowledge bases are queried via Azure AI Search agentic retrieval endpoints.

## Operations

### Query a knowledge base (agentic retrieval)

```bash
curl -s -X POST "http://localhost:8443/knowledgebases/${KB_NAME}/retrieve?api-version=v1" \
  -H "Content-Type: application/json" \
  -d '{"query": "What does the Q3 report say about revenue?", "retrieval_mode": "agentic"}'
```

Returns results with citations referencing source documents, chunks, and page numbers.

### Simple search (keyword/vector/hybrid)

```bash
curl -s -X POST "http://localhost:8443/knowledgebases/${KB_NAME}/search?api-version=v1" \
  -H "Content-Type: application/json" \
  -d '{"search": "revenue Q3", "queryType": "semantic", "top": 5}'
```

### List knowledge bases

```bash
curl -s "http://localhost:8443/knowledgebases?api-version=v1"
```

## When to use

- User asks questions about organizational documents: "what does the report say about X"
- You need to ground answers in enterprise data with citations
- User says "search my documents", "find in the knowledge base"
- Answering questions that require multiple document sources

## When NOT to use

- For real-time web information (use foundry-web-search)
- For running code or calculations (use foundry-code)
- For simple text files in /sandbox/ (just read them directly)
- For remembering user preferences (use foundry-memory)

## Knowledge sources supported

- Azure Blob Storage (PDFs, docs, text)
- SharePoint document libraries
- Microsoft OneLake
- Public web content
