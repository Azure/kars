// kars Bridge — Teams Gateway: configuration (fail-closed).
//
// All identity and routing configuration is required. The gateway refuses to
// start if any field is missing or malformed.

export interface EntraRoleMapping {
  readonly entraSubject: string;
  readonly bridgeSubject: string;
  readonly bridgeRoles: readonly string[];
  readonly displayName: string;
}

export interface TeamsGatewayConfig {
  readonly clientId: string;
  readonly clientSecret: string;
  readonly tenantId: string;
  readonly entraRoleMappings: readonly EntraRoleMapping[];
  readonly bffBaseUrl: string;
  readonly bffInternalSecret: string;
  readonly port: number;
  readonly internalPort: number;
  readonly conversationConfigMapNamespace: string;
  readonly conversationConfigMapName: string;
  readonly watchNamespace: string;
}

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value || value.trim().length === 0) {
    throw new Error(
      `FATAL: required environment variable ${name} is not set. ` +
        `The Teams gateway refuses to start without complete identity configuration (fail-closed).`
    );
  }
  return value.trim();
}

/**
 * Parse TEAMS_ENTRA_ROLE_MAP JSON:
 * [{"entra_subject":"<oid>","bridge_subject":"<oidc-sub>","roles":["operator","user"],"name":"Alice"}]
 *
 * Every subject must have at least one role. Fail closed if empty or malformed.
 */
function parseRoleMappings(raw: string): EntraRoleMapping[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error(
      "FATAL: TEAMS_ENTRA_ROLE_MAP is not valid JSON. Expected an array of {subject, roles, name}."
    );
  }
  if (!Array.isArray(parsed) || parsed.length === 0) {
    throw new Error(
      "FATAL: TEAMS_ENTRA_ROLE_MAP must be a non-empty JSON array."
    );
  }
  const mappings: EntraRoleMapping[] = [];
  for (const entry of parsed) {
    if (
      typeof entry !== "object" ||
      entry === null ||
      typeof entry.entra_subject !== "string" ||
      !entry.entra_subject.trim() ||
      typeof entry.bridge_subject !== "string" ||
      !entry.bridge_subject.trim() ||
      !Array.isArray(entry.roles) ||
      entry.roles.length === 0 ||
      typeof entry.name !== "string" ||
      !entry.name.trim()
    ) {
      throw new Error(
        `FATAL: TEAMS_ENTRA_ROLE_MAP entry is malformed: ${JSON.stringify(entry)}. ` +
          `Each entry must have non-empty "entra_subject", "bridge_subject", "roles" (array), and "name".`
      );
    }
    for (const role of entry.roles) {
      if (typeof role !== "string" || !role.trim()) {
        throw new Error(
          `FATAL: TEAMS_ENTRA_ROLE_MAP entry for ${entry.entra_subject} has invalid role.`
        );
      }
    }
    mappings.push({
      entraSubject: entry.entra_subject.trim(),
      bridgeSubject: entry.bridge_subject.trim(),
      bridgeRoles: entry.roles.map((r: string) => r.trim()),
      displayName: entry.name.trim(),
    });
  }
  return mappings;
}

export function loadConfig(): TeamsGatewayConfig {
  const clientId = requireEnv("TEAMS_CLIENT_ID");
  const clientSecret = requireEnv("TEAMS_CLIENT_SECRET");
  const tenantId = requireEnv("TEAMS_TENANT_ID");
  const bffBaseUrl = requireEnv("TEAMS_BFF_BASE_URL");
  const bffInternalSecret = requireEnv("TEAMS_BFF_INTERNAL_SECRET");

  const rawRoleMap = requireEnv("TEAMS_ENTRA_ROLE_MAP");
  const entraRoleMappings = parseRoleMappings(rawRoleMap);

  const port = parseInt(process.env["TEAMS_GATEWAY_PORT"] ?? "3978", 10);
  const internalPort = port + 1;
  const conversationConfigMapNamespace =
    process.env["TEAMS_CONFIGMAP_NAMESPACE"]?.trim() || "kars-system";
  const conversationConfigMapName =
    process.env["TEAMS_CONFIGMAP_NAME"]?.trim() || "kars-teams-conversations";
  const watchNamespace =
    process.env["TEAMS_WATCH_NAMESPACE"]?.trim() || "kars-system";

  return {
    clientId,
    clientSecret,
    tenantId,
    entraRoleMappings,
    bffBaseUrl,
    bffInternalSecret,
    port,
    internalPort,
    conversationConfigMapNamespace,
    conversationConfigMapName,
    watchNamespace,
  };
}

export { parseRoleMappings };
