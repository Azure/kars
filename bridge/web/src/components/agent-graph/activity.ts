import type { ActivityEvent } from "@/lib/types";
import type { AgentAction, AgentExecution, ToolEvent } from "./types";

export function normalize(value: string): string {
  return value.trim().toLowerCase();
}

function humanizeTool(name: string): string {
  const tool = name.toLowerCase().replaceAll("-", "_");
  if (/(browser_)?navigate|open_url|goto/.test(tool)) return "Opened a browser page";
  if (/screenshot|capture_screen/.test(tool)) return "Captured a screenshot";
  if (/browser_(click|dblclick)|click_element/.test(tool)) return "Clicked a page control";
  if (/fill_form|browser_fill|select_option/.test(tool)) return "Filled in a form";
  if (/browser_type|press_key|keyboard/.test(tool)) return "Entered text on a page";
  if (/browser_snapshot|page_snapshot|accessibility_tree/.test(tool)) return "Inspected a browser page";
  if (/browser_wait|wait_for/.test(tool)) return "Waited for a page update";
  if (/network_request|network_requests/.test(tool)) return "Inspected browser network activity";
  if (/file_upload/.test(tool)) return "Uploaded a file";
  if (/(web_)?search|brave|tavily|exa|perplexity/.test(tool)) return "Searched the web";
  if (/fetch|http|curl|crawl|download/.test(tool)) return "Retrieved network content";
  if (/pull_request|create_pr|open_pr/.test(tool)) return "Worked with a pull request";
  if (/git|commit|branch|push|pull/.test(tool)) return "Worked with source control";
  if (/write|create_file|save|edit|patch|append/.test(tool)) return "Updated a file";
  if (/read|view|list|glob|grep|find|search_files/.test(tool)) return "Inspected files";
  if (/shell|bash|exec|run_command|terminal/.test(tool)) return "Ran a command";
  if (/message|handoff|send|relay/.test(tool)) return "Sent an agent message";
  return "Used a governed tool";
}

function meaningfulDestination(host: string): boolean {
  const value = host.toLowerCase().replace(/^\[|\]$/g, "");
  return !/^(localhost|0\.0\.0\.0|127(?:\.\d+){3}|::1)(:\d+)?$/.test(value);
}

export function destinationsFrom(event: ToolEvent): string[] {
  const values = `${event.args_preview} ${event.result_preview}`;
  const destinations = new Set<string>();
  for (const match of values.matchAll(/https?:\/\/([^/\s"')\]]+)/gi)) {
    const host = match[1].toLowerCase().replace(/[.,;]+$/, "");
    if (meaningfulDestination(host)) destinations.add(host);
  }
  return [...destinations];
}

export function formatLastActivity(value: string | null): string {
  if (!value) return "No recorded activity";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const months = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  const pad = (part: number) => String(part).padStart(2, "0");
  return `${months[date.getUTCMonth()]} ${date.getUTCDate()}, ${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())} UTC`;
}

export function phaseKind(phase: string): "active" | "failed" | "paused" | "complete" | "idle" {
  const value = phase.toLowerCase();
  if (/(failed|degraded|error|blocked)/.test(value)) return "failed";
  if (/(running|launching|active|working|executing)/.test(value)) return "active";
  if (/(paused|hibernating|suspended|waiting)/.test(value)) return "paused";
  if (/(completed|finished|succeeded|delivered)/.test(value)) return "complete";
  return "idle";
}

export function phaseTone(phase: string): string {
  switch (phaseKind(phase)) {
    case "failed":
      return "border-danger/35 bg-danger/10 text-danger";
    case "active":
      return "border-signal/35 bg-signal/10 text-signal";
    case "paused":
      return "border-accent/35 bg-accent/10 text-accent";
    case "complete":
      return "border-signal/25 bg-signal/[0.06] text-foreground-muted";
    default:
      return "border-border bg-surface-muted text-foreground-muted";
  }
}

export function actionFromEvent(event: ActivityEvent, index: number): AgentAction {
  if (event.kind === "round") {
    return {
      id: `round-${event.agent ?? "principal"}-${event.round}-${index}`,
      human: `Completed model round ${event.round + 1}`,
      raw: "model.round",
      args: `${event.tool_calls} tool call${event.tool_calls === 1 ? "" : "s"} requested`,
      result: `${event.finish_reason || "unknown finish"}; ${event.total_tokens.toLocaleString("en-US")} tokens`,
      ok: null,
      round: event.round,
      ms: event.ms,
      ts: event.ts,
      seq: event.seq ?? null,
      agentInstance: event.agentInstance ?? null,
    };
  }
  return {
    id: `tool-${event.agent ?? "principal"}-${event.round}-${index}`,
    human: humanizeTool(event.name),
    raw: event.name,
    args: event.args_preview,
    result: event.result_preview,
    ok: event.ok,
    round: event.round,
    ms: event.ms,
    ts: event.ts,
    seq: event.seq ?? null,
    agentInstance: event.agentInstance ?? null,
  };
}

export function ellipsis(value: string, length: number): string {
  return value.length > length ? `${value.slice(0, length - 1)}…` : value;
}

function queryAgentIdentity(agent: AgentExecution): string {
  return agent.isPrincipal
    ? "principal"
    : normalize(agent.observedAgentName ?? agent.technicalName);
}

export function exactActionQuery(agent: AgentExecution, action: AgentAction): string {
  const parameters = new URLSearchParams({
    agent: queryAgentIdentity(agent),
    round: String(action.round),
    ts: action.ts,
    tool: action.raw,
    args: action.args,
    result: action.result,
  });
  if (action.seq != null) parameters.set("seq", String(action.seq));
  if (action.agentInstance) parameters.set("instance", action.agentInstance);
  return `action:${parameters.toString()}`;
}

export function exactActionsQuery(agent: AgentExecution, actions: AgentAction[]): string {
  const through = actions.at(-1)?.ts ?? "";
  const parameters = new URLSearchParams({
    agent: queryAgentIdentity(agent),
    through,
  });
  const sequenceValues = actions.flatMap((action) =>
    action.seq == null ? [] : [String(action.seq)]
  );
  if (sequenceValues.length === actions.length && sequenceValues.length > 0) {
    parameters.set("seqs", sequenceValues.join(","));
  } else {
    parameters.set(
      "events",
      JSON.stringify(actions.map((action) => ({
        round: action.round,
        ts: action.ts,
        tool: action.raw,
        args: action.args,
        result: action.result,
        instance: action.agentInstance,
      }))),
    );
  }
  const instances = [...new Set(actions.flatMap((action) =>
    action.agentInstance ? [action.agentInstance] : []
  ))];
  if (instances.length === 1) parameters.set("instance", instances[0]);
  return `actions:${parameters.toString()}`;
}
