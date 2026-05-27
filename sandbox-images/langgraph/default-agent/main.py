"""
Kars default agent for the LangGraph (Python) runtime.

Builds a minimal single-node LangGraph that calls the router-pinned
chat model. Replace this file by mounting agent code via `agentCode`.
"""

from __future__ import annotations

import os
import sys
import time

# Bootstrap MUST run in this process so OPENAI_BASE_URL /
# OPENAI_API_KEY (router-managed sentinel) and OTel are set
# before any framework SDK is imported.
from kars_runtime_langgraph.runtime import bootstrap
bootstrap()

BANNER = "🔒 Kars — LangGraph (default agent)"


def _env(name: str, default: str = "(unset)") -> str:
    v = os.environ.get(name)
    return v if v else default


def _print_banner() -> None:
    print("=" * 64, flush=True)
    print(BANNER, flush=True)
    print("=" * 64, flush=True)
    print(f"  sandbox        : {_env('SANDBOX_NAME', _env('HOSTNAME'))}", flush=True)
    print(f"  model          : {_env('AZURE_OPENAI_DEPLOYMENT', _env('OPENCLAW_MODEL'))}", flush=True)
    print(f"  router         : {_env('OPENAI_BASE_URL', 'http://127.0.0.1:8443/v1')}", flush=True)
    print(f"  foundry project: {_env('FOUNDRY_PROJECT_ENDPOINT')}", flush=True)
    print(f"  agt relay      : {_env('AGT_RELAY_URL')}", flush=True)
    print("=" * 64, flush=True)


def _smoke_test() -> None:
    try:
        from langchain_openai import ChatOpenAI  # type: ignore
        from langgraph.graph import END, START, MessagesState, StateGraph  # type: ignore
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ langgraph/langchain import failed: {exc}", flush=True)
        return

    model = os.environ.get("AZURE_OPENAI_DEPLOYMENT") or os.environ.get("OPENCLAW_MODEL") or "gpt-4.1"
    print(f"[default-agent] running smoke test against model={model}…", flush=True)

    try:
        llm = ChatOpenAI(model=model)

        def call_model(state: MessagesState) -> dict:
            return {"messages": [llm.invoke(state["messages"])]}

        graph = StateGraph(MessagesState)
        graph.add_node("agent", call_model)
        graph.add_edge(START, "agent")
        graph.add_edge("agent", END)
        compiled = graph.compile()

        result = compiled.invoke({
            "messages": [
                ("system", "You are a default smoke-test agent embedded in an Kars sandbox."),
                ("user", "Reply with exactly one short sentence confirming you are alive."),
            ],
        })
        last = result["messages"][-1]
        content = getattr(last, "content", str(last))
        print(f"[default-agent] ✓ inference reply: {content}", flush=True)
        print("[default-agent] ✓ Foundry inference proven via inference-router", flush=True)
    except Exception as exc:  # noqa: BLE001
        print(f"[default-agent] ✗ smoke test failed: {exc}", flush=True)


def _idle_forever() -> None:
    print("[default-agent] idling — supply your own /sandbox/agent/main.py to take over.", flush=True)
    while True:
        time.sleep(3600)


def main() -> int:
    _print_banner()
    _smoke_test()
    _idle_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
