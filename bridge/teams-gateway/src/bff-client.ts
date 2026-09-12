import type { TeamsGatewayConfig } from "./config.js";
import type { CardVerdict } from "./cards.js";
import type { ResolvedPrincipal } from "./identity.js";
import { log } from "./log.js";

export interface DecisionRequest {
  readonly approvalName: string;
  readonly approvalNamespace: string;
  readonly verdict: CardVerdict;
  readonly reason?: string | undefined;
  readonly resourceVersion: string;
  readonly boundEnvelopeDigest?: string | undefined;
  readonly principal: ResolvedPrincipal;
}

export interface DecisionResponse {
  readonly success: boolean;
  readonly phase?: string | undefined;
  readonly error?: string | undefined;
}

export type TeamCommandName =
  | "bind"
  | "status"
  | "list-tasks"
  | "add-task"
  | "run"
  | "halt";

export interface TeamCommandRequest {
  readonly teamName: string;
  readonly namespace: string;
  readonly command: TeamCommandName;
  readonly args: string;
  readonly principal: ResolvedPrincipal;
}

export interface TeamCommandResponse {
  readonly success: boolean;
  readonly message: string;
  readonly error?: string | undefined;
}

export class BffClient {
  private readonly baseUrl: string;
  private readonly secret: string;

  public constructor(config: TeamsGatewayConfig) {
    this.baseUrl = config.bffBaseUrl.replace(/\/$/, "");
    this.secret = config.bffInternalSecret;
  }

  public async submitDecision(
    request: DecisionRequest
  ): Promise<DecisionResponse> {
    const response = await this.postJson(
      "/api/internal/teams/decision",
      {
        approval_name: request.approvalName,
        approval_namespace: request.approvalNamespace,
        verdict: request.verdict,
        resource_version: request.resourceVersion,
        ...(request.reason && request.reason.trim().length > 0
          ? { reason: request.reason.trim() }
          : {}),
        ...(request.boundEnvelopeDigest !== undefined
          ? { bound_envelope_digest: request.boundEnvelopeDigest }
          : {}),
        entra_subject: request.principal.entraSubject,
        entra_name: request.principal.name,
      }
    );
    if (!response.success) {
      return response;
    }
    const payload = response.json as { phase?: unknown };
    return {
      success: true,
      phase:
        typeof payload.phase === "string" ? payload.phase : undefined,
    };
  }

  public async sendTeamCommand(
    request: TeamCommandRequest
  ): Promise<TeamCommandResponse> {
    const response = await this.postJson(
      "/api/internal/teams/command",
      {
        team_name: request.teamName,
        namespace: request.namespace,
        command: request.command,
        args: request.args,
        entra_subject: request.principal.entraSubject,
        entra_name: request.principal.name,
      }
    );
    if (!response.success) {
      return {
        success: false,
        message: "",
        error: response.error,
      };
    }
    const payload = response.json as { message?: unknown };
    return {
      success: true,
      message:
        typeof payload.message === "string"
          ? payload.message
          : "Command completed.",
    };
  }

  private async postJson(
    path: string,
    body: Record<string, unknown>
  ): Promise<
    | {
        readonly success: true;
        readonly json: unknown;
      }
    | {
        readonly success: false;
        readonly error: string;
      }
  > {
    const url = `${this.baseUrl}${path}`;
    try {
      const response = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-Teams-Internal-Secret": this.secret,
        },
        body: JSON.stringify(body),
      });
      if (!response.ok) {
        const error = await response.text();
        log("error", "BFF request failed", {
          url,
          status: String(response.status),
          error,
        });
        return {
          success: false,
          error,
        };
      }
      return {
        success: true,
        json: (await response.json()) as unknown,
      };
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      log("error", "BFF request failed", { url, error: message });
      return {
        success: false,
        error: message,
      };
    }
  }
}
