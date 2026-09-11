"use server";

// kars Bridge — set the DEV role-simulation cookie. No SSO yet, so this lets one
// developer view the Bridge as each role and verify the gates hold. When an auth
// proxy is added it sets this (or a signed header) from verified group claims.

import { cookies } from "next/headers";
import { redirect } from "next/navigation";
import { ALL_ROLES, type Role } from "@/lib/config";
import { ssoConfigured } from "@/lib/oidc-config";

export async function switchRole(role: string): Promise<void> {
  // Hard-disable the dev role switch once a real IdP is configured — otherwise
  // any user could set `bridge-role` to `admin` and self-escalate. Under SSO the
  // only source of roles is the signed session (see lib/session.ts).
  if (ssoConfigured()) {
    redirect("/workspace");
  }
  const jar = await cookies();
  if (role === "reset" || !(ALL_ROLES as string[]).includes(role)) {
    jar.delete("bridge-role");
  } else {
    jar.set("bridge-role", role as Role, {
      httpOnly: true,
      sameSite: "lax",
      path: "/",
      maxAge: 60 * 60 * 24 * 7,
    });
  }
  // Land on the home each role should see, so switching feels like signing in.
  const home =
    role === "operator" || role === "admin"
      ? "/console"
      : role === "auditor"
        ? "/audit"
        : "/workspace";
  redirect(home);
}
