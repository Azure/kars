// kars Bridge — honest landing for a real SSO login that mapped to zero
// roles. Fail-closed by design (lib/oidc.ts rolesFromClaims): an
// unrecognized or absent role claim never grants a default role.

export const dynamic = "force-dynamic";

export default function NoRolesPage() {
  return (
    <div className="mx-auto max-w-lg space-y-4 px-6 py-16 text-center">
      <h1 className="text-xl font-semibold">Signed in — no roles mapped</h1>
      <p className="text-sm text-foreground-muted">
        Your identity provider login succeeded, but none of your group/role claims are mapped to a Bridge role.
        Roles are fail-closed by design: an operator must add your IdP group to{" "}
        <code className="font-mono text-xs">BRIDGE_OIDC_ROLE_MAP</code> before you can use the Bridge.
      </p>
      <p className="text-xs text-foreground-muted">
        Ask an operator to map your group, then sign in again at{" "}
        <a href="/auth/login" className="text-signal underline">
          /auth/login
        </a>
        .
      </p>
    </div>
  );
}
