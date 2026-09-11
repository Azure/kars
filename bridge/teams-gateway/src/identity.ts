import type { TeamsGatewayConfig } from "./config.js";
import { log } from "./log.js";

export interface TeamsIdentity {
  readonly aadObjectId: string;
  readonly displayName: string;
}

export interface ResolvedPrincipal {
  readonly entraSubject: string;
  readonly sub: string;
  readonly name: string;
  readonly roles: readonly string[];
}

export function resolveIdentity(
  config: TeamsGatewayConfig,
  identity: TeamsIdentity | undefined
): ResolvedPrincipal | null {
  const subject = identity?.aadObjectId.trim();
  if (!subject) {
    log("warn", "rejecting Teams activity without Entra subject");
    return null;
  }

  const mapping = config.entraRoleMappings.find(
    (entry) => entry.entraSubject === subject
  );
  if (!mapping) {
    log("warn", "rejecting Teams activity from unmapped Entra subject", {
      aadObjectId: subject,
      displayName: identity?.displayName ?? "unknown",
    });
    return null;
  }

  return {
    entraSubject: mapping.entraSubject,
    sub: mapping.bridgeSubject,
    name: mapping.displayName,
    roles: [...mapping.bridgeRoles],
  };
}
