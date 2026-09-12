"use client";

// kars Bridge — the multi-user surface. Shows the current principal + primary
// role, and (no SSO yet) lets a developer switch which role they act as, so the
// four differentiated permission sets — admin / operator / workspace user /
// auditor — can be exercised and verified. The dropdown is honest about being a
// dev identity switch, not a logged-in session.

import { useState } from "react";
import { switchRole } from "@/app/role-actions";
import { ALL_ROLES, ROLE_META, type Role } from "@/lib/config";
import { Icon } from "@/components/icon";

export function RoleSwitcher({
  principal,
  primary,
  roles,
  simulated,
  ssoSignedIn = false,
  ssoAvailable = false,
}: {
  principal: string;
  primary: Role;
  roles: Role[];
  simulated: boolean;
  /** True when the CURRENT session is a real, signed, SSO-verified login. */
  ssoSignedIn?: boolean;
  /** True when an operator has configured a real OIDC IdP (BRIDGE_OIDC_*),
   *  regardless of whether THIS request is signed in yet. */
  ssoAvailable?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const meta = ROLE_META[primary];

  return (
    <div className="relative">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="inline-flex items-center gap-1.5 rounded-lg border border-border bg-surface px-2.5 py-1 text-xs font-medium text-foreground-muted transition hover:bg-surface-muted hover:text-foreground"
        title={
          ssoSignedIn
            ? `${principal} — acting as ${meta.label} (signed in via SSO)`
            : `Acting as ${meta.label}${simulated ? " (simulated — no SSO)" : ""}`
        }
      >
        <Icon name={meta.glyph} size={14} />
        {ssoSignedIn ? (
          // Real login: show WHO you are (the identity), with the role conveyed
          // by the glyph + the dropdown. A signed-in user must see their own
          // identity in the header, not just their role.
          <span className="hidden max-w-[12rem] truncate sm:inline">{principal}</span>
        ) : (
          <span className="hidden sm:inline">{meta.label}</span>
        )}
        <svg viewBox="0 0 12 12" className="h-2.5 w-2.5" fill="currentColor" aria-hidden>
          <path d="M6 8 2 4h8L6 8Z" />
        </svg>
      </button>
      {open && (
        <>
          <button
            type="button"
            aria-hidden
            className="fixed inset-0 z-10 cursor-default"
            onClick={() => setOpen(false)}
          />
          <div className="absolute right-0 z-20 mt-1.5 w-72 rounded-xl border border-border bg-surface p-2 shadow-lg">
            <div className="px-2 py-1.5">
              <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Signed in as</p>
              <p className="truncate font-mono text-xs">{principal}</p>
              <p className="mt-0.5 text-[10px] text-foreground-muted">
                {ssoSignedIn
                  ? "Real SSO session — roles from your identity provider's group claims."
                  : simulated
                    ? "Simulated role — there is no SSO session."
                    : "Roles from BRIDGE_ROLES env."}{" "}
                The real boundary is the Bridge&rsquo;s Kubernetes ServiceAccount.
              </p>
            </div>
            <div className="my-1 border-t border-border" />
            {ssoSignedIn ? (
              <form action="/auth/logout" method="post">
                <button
                  type="submit"
                  className="w-full rounded-lg border border-border px-2 py-1.5 text-left text-[11px] font-medium text-foreground-muted hover:bg-surface-muted"
                >
                  Sign out
                </button>
              </form>
            ) : (
              <>
                {ssoAvailable && (
                  <a
                    href="/auth/login"
                    className="mb-1 block rounded-lg bg-signal px-2 py-1.5 text-center text-[11px] font-semibold text-signal-fg hover:opacity-90"
                  >
                    Sign in with SSO
                  </a>
                )}
                <p className="px-2 pb-1 text-[10px] font-medium uppercase tracking-wide text-foreground-muted">
                  Act as {ssoAvailable && "(dev preview — no session)"}
                </p>
                {ALL_ROLES.map((r) => {
                  const m = ROLE_META[r];
                  const isPrimary = r === primary;
                  return (
                    <form key={r} action={switchRole.bind(null, r)}>
                      <button
                        type="submit"
                        className={`flex w-full items-start gap-2 rounded-lg px-2 py-1.5 text-left transition hover:bg-surface-muted ${
                          isPrimary ? "bg-surface-muted/60" : ""
                        }`}
                      >
                        <span aria-hidden className="mt-0.5"><Icon name={m.glyph} size={15} /></span>
                        <span className="min-w-0">
                          <span className="flex items-center gap-1.5 text-xs font-medium">
                            {m.label}
                            {isPrimary && (
                              <span className="rounded-full border border-signal/40 bg-signal/10 px-1.5 py-0 text-[9px] text-signal">
                                current
                              </span>
                            )}
                          </span>
                          <span className="block text-[10px] leading-snug text-foreground-muted">{m.blurb}</span>
                        </span>
                      </button>
                    </form>
                  );
                })}
                {simulated && (
                  <form action={switchRole.bind(null, "reset")}>
                    <button
                      type="submit"
                      className="mt-1 w-full rounded-lg border border-border px-2 py-1.5 text-[11px] text-foreground-muted hover:bg-surface-muted"
                    >
                      Reset to env default
                    </button>
                  </form>
                )}
              </>
            )}
          </div>
        </>
      )}
    </div>
  );
}
