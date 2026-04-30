/**
 * Runtime-aware helpers for CLI commands (S10.A5).
 *
 * The controller's `RuntimeKind` enum is the source of truth for which
 * runtimes the platform supports. The CLI only needs three derived
 * facts: which container name to address with `kubectl exec -c`, which
 * `spec.runtime.{variant}` block to emit when scaffolding a CR, and
 * what user-facing label to print in `list` / `status` output.
 *
 * Mirrors `controller::reconciler::runtime::kind_str` and
 * `controller::reconciler::mod::is_openclaw` so the CLI does not drift
 * from the controller. Keep the two enums in lockstep.
 */

/** PascalCase wire-format kinds matching `controller/src/crd.rs::RuntimeKind`. */
export type RuntimeKind =
  | "OpenClaw"
  | "OpenAIAgents"
  | "MicrosoftAgentFramework"
  | "SemanticKernel"
  | "LangGraph"
  | "Anthropic"
  | "BYO";

/** kebab-case CLI flag values (e.g. `--runtime openai-agents`). */
export type RuntimeFlag =
  | "openclaw"
  | "openai-agents"
  | "microsoft-agent-framework"
  | "semantic-kernel"
  | "lang-graph"
  | "anthropic"
  | "byo";

const FLAG_TO_KIND: Record<RuntimeFlag, RuntimeKind> = {
  "openclaw": "OpenClaw",
  "openai-agents": "OpenAIAgents",
  "microsoft-agent-framework": "MicrosoftAgentFramework",
  "semantic-kernel": "SemanticKernel",
  "lang-graph": "LangGraph",
  "anthropic": "Anthropic",
  "byo": "BYO",
};

/** Runtimes whose **adapter is wired end-to-end** in the current controller build. */
const WIRED_KINDS: RuntimeKind[] = [
  "OpenClaw",
  "OpenAIAgents",
  "MicrosoftAgentFramework",
  "BYO",
];

export function flagToKind(flag: string): RuntimeKind {
  const k = FLAG_TO_KIND[flag.toLowerCase() as RuntimeFlag];
  if (!k) {
    throw new Error(
      `Unknown --runtime value: ${flag}. ` +
      `Valid values: ${Object.keys(FLAG_TO_KIND).join(", ")}.`,
    );
  }
  return k;
}

/**
 * Reject Tier-2 placeholders (SemanticKernel/LangGraph/Anthropic) at
 * the CLI boundary so the operator gets a clear "not yet wired" error
 * rather than a `RuntimeReady=False / AdapterMissing` Condition stamp
 * after the apply round-trips. MAF .NET also unwired (Phase 3).
 */
export function assertRuntimeWired(kind: RuntimeKind): void {
  if (!WIRED_KINDS.includes(kind)) {
    throw new Error(
      `Runtime kind '${kind}' has no adapter wired in this controller build. ` +
      `Wired runtimes: ${WIRED_KINDS.join(", ")}. ` +
      `Tier-2 runtimes (${(["SemanticKernel", "LangGraph", "Anthropic"] as const).join(", ")}) ` +
      `are roadmap items pending operator demand.`,
    );
  }
}

/**
 * Container name for `kubectl exec -c <name>` etc. OpenClaw uses
 * `openclaw` (legacy); every non-OpenClaw runtime uses the generic
 * `agent` name (mirrors `is_openclaw` polarity in the reconciler:
 * `controller::reconciler::mod::is_openclaw` selects "openclaw" /
 * "agent" identically).
 */
export function agentContainerName(kind: RuntimeKind): string {
  return kind === "OpenClaw" ? "openclaw" : "agent";
}

/**
 * Resolve the runtime kind from a `ClawSandbox` CR (live or YAML).
 * `spec.runtime.kind` is the canonical field after the S10.A1
 * discriminated-union migration; pre-A1 CRs that have no
 * `spec.runtime` block fall back to `OpenClaw` (the only legal value
 * before A1).
 */
export function runtimeKindFromCr(cr: unknown): RuntimeKind {
  const spec = (cr as { spec?: { runtime?: { kind?: string } } } | undefined)?.spec;
  const kind = spec?.runtime?.kind;
  if (kind && Object.values(FLAG_TO_KIND).includes(kind as RuntimeKind)) {
    return kind as RuntimeKind;
  }
  return "OpenClaw";
}

/**
 * Build the `spec.runtime` block for a new ClawSandbox CR based on
 * CLI options. Returns the runtime block to splice into the CR
 * scaffold; callers are responsible for the surrounding spec scaffolding.
 *
 * `byoImage` is **required** for `kind: BYO`; passing it for any
 * other kind is silently ignored (the CLI flag is BYO-specific by
 * design — no runtime-specific image override for the wired runtimes
 * because their adapter image is controller-managed).
 */
export interface BuildRuntimeBlockOpts {
  kind: RuntimeKind;
  /** OpenClaw only — version pin from the legacy CLI default. */
  openclawVersion?: string;
  /** OpenClaw only — model name (becomes `azure/<model>`). */
  model?: string;
  /** Optional sandbox image override (OpenClaw legacy `--image`). */
  image?: string;
  /** BYO only — container image (REQUIRED for `kind: BYO`). */
  byoImage?: string;
  /** BYO only — contract version label. */
  byoContractVersion?: string;
  /** MAF only — language flavour. Defaults to "python". */
  mafLanguage?: "python" | "dotnet";
}

export function buildRuntimeBlock(
  opts: BuildRuntimeBlockOpts,
): Record<string, unknown> {
  switch (opts.kind) {
    case "OpenClaw":
      return {
        kind: "OpenClaw",
        openclaw: {
          version: opts.openclawVersion ?? "2026.3.13",
          ...(opts.image ? { image: opts.image } : {}),
          config: {
            agent: {
              model: `azure/${opts.model ?? "gpt-4.1"}`,
            },
          },
        },
      };
    case "OpenAIAgents":
      return {
        kind: "OpenAIAgents",
        openaiAgents: {},
      };
    case "MicrosoftAgentFramework": {
      const language = opts.mafLanguage ?? "python";
      // Reject dotnet client-side: the controller producer rejects
      // it via `ShapeInvalid`, but failing here gives the operator
      // immediate feedback rather than a Conditions-stamp round-trip.
      if (language !== "python") {
        throw new Error(
          `MAF language '${language}' is not yet wired ` +
          `(blocked on AgentMesh.Sdk .NET upstream — Phase 3). ` +
          `Use --maf-language python.`,
        );
      }
      return {
        kind: "MicrosoftAgentFramework",
        microsoftAgentFramework: { language: "python" },
      };
    }
    case "BYO": {
      if (!opts.byoImage) {
        throw new Error(
          `--byo-image is required when --runtime byo. ` +
          `The image must declare the org.azureclaw.runtime.contract=v1 label.`,
        );
      }
      return {
        kind: "BYO",
        byo: {
          image: opts.byoImage,
          contractVersion: opts.byoContractVersion ?? "v1",
        },
      };
    }
    default:
      // Defensive — `assertRuntimeWired` should have caught this.
      throw new Error(`Runtime kind '${opts.kind}' is not wired.`);
  }
}
