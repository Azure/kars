---
name: foundry-conversations
description: Manage persistent conversations via Foundry Conversations API. Create conversations, add messages, and maintain history across sessions.
metadata: {"openclaw": {"requires": {"env": ["FOUNDRY_PROJECT_ENDPOINT"]}, "primaryEnv": "FOUNDRY_PROJECT_ENDPOINT"}}
---

# Foundry Conversations — Persistent Conversation Management

You can create and manage persistent conversations via the Foundry Conversations API. Conversations store message history server-side, enabling multi-turn interactions that survive session boundaries.

## Endpoint

All requests: `http://localhost:8443` with `?api-version=2025-11-15-preview`. Auth is automatic.

## Operations

### Create a conversation

```bash
curl -s -X POST 'http://localhost:8443/openai/conversations?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"metadata":{"user":"user-123","topic":"onboarding"}}'
```

Returns `{"id":"conv_abc123","object":"conversation",...}`.

### Add messages to a conversation

```bash
curl -s -X POST 'http://localhost:8443/openai/conversations/conv_abc123/items?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"items":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Hello!"}]}]}'
```

### List conversations

```bash
curl -s 'http://localhost:8443/openai/conversations?api-version=2025-11-15-preview'
```

### Generate a response in a conversation context

```bash
curl -s -X POST 'http://localhost:8443/openai/responses?api-version=2025-11-15-preview' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4.1","input":"What did we discuss?","conversation":"conv_abc123"}'
```

### Delete a conversation

```bash
curl -s -X DELETE 'http://localhost:8443/openai/conversations/conv_abc123?api-version=2025-11-15-preview'
```

## When to use

- Multi-turn conversations where server-side history is needed
- Building chat applications with conversation persistence
- When you need to reference previous messages by conversation ID

## When NOT to use

- For stateless one-off queries (use /v1/chat/completions)
- For long-term user preferences (use foundry-memory skill)
