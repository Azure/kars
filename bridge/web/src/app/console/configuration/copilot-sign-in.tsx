// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useEffect, useRef, useState } from "react";
import { copilotLoginStartAction, copilotLoginPollAction } from "./copilot-login-actions";
import { Icon } from "@/components/icon";
import type { DiscoveredModel } from "@/lib/types";
import type { CopilotLoginStart, CopilotPendingReason } from "@/lib/bff-contracts";
import { createCopilotLoginController } from "@/lib/copilot-login";

export function CopilotSignIn({
  signedIn,
  onAuthorized,
}: {
  signedIn: boolean;
  onAuthorized: (models: DiscoveredModel[]) => void;
}) {
  const [starting, setStarting] = useState(false);
  const [flow, setFlow] = useState<CopilotLoginStart | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [pending, setPending] = useState<CopilotPendingReason | undefined>();
  const controller = useRef<ReturnType<typeof createCopilotLoginController> | null>(null);
  const authorized = useRef(onAuthorized);
  useEffect(() => { authorized.current = onAuthorized; }, [onAuthorized]);

  useEffect(() => {
    const current = createCopilotLoginController({
      start: copilotLoginStartAction, poll: copilotLoginPollAction,
    }, {
      flow: setFlow, starting: setStarting, error: setError, pending: setPending,
      authorized: models => authorized.current(models),
    });
    controller.current = current;
    return () => { current.dispose(); controller.current = null; };
  }, []);

  useEffect(() => { if (signedIn) controller.current?.cancel(); }, [signedIn]);

  if (signedIn) {
    return (
      <p className="flex items-center gap-1.5 text-xs text-signal">
        <Icon name="check" size={13} /> Signed in to GitHub Copilot — your seat&rsquo;s models are listed in the next step.
      </p>
    );
  }

  function begin() {
    setCopied(false);
    void controller.current?.begin();
  }

  if (!flow) {
    return (
      <div className="space-y-2">
        <p className="text-xs text-foreground-muted">
          Sign in with your GitHub account to verify your Copilot seat and load the exact models it serves. No token to paste — it&rsquo;s minted and stored securely on the cluster.
        </p>
        <button type="button" onClick={begin} disabled={starting} className="inline-flex items-center gap-1.5 rounded-lg bg-signal px-3 py-2 text-xs font-semibold text-signal-fg disabled:opacity-50">
          <Icon name="link" size={13} /> {starting ? "Starting…" : "Sign in with GitHub"}
        </button>
        {starting && <button type="button" onClick={() => controller.current?.cancel()} className="ml-2 text-xs text-foreground-muted">Cancel sign-in</button>}
        {error && <p className="text-[11px] text-danger">{error}</p>}
      </div>
    );
  }

  return (
    <div className="space-y-2.5">
      <p className="text-xs text-foreground-muted">Finish signing in on GitHub:</p>
      <ol className="space-y-2 text-xs">
        <li className="flex items-center gap-2">
          <span className="grid h-5 w-5 place-items-center rounded-full bg-signal/15 text-[10px] text-signal">1</span>
          <span>Open <a href={flow.verification_uri} target="_blank" rel="noreferrer" className="font-medium text-signal hover:underline">{flow.verification_uri} ↗</a></span>
        </li>
        <li className="flex items-center gap-2">
          <span className="grid h-5 w-5 place-items-center rounded-full bg-signal/15 text-[10px] text-signal">2</span>
          <span className="flex items-center gap-2">
            Enter code
            <code className="rounded border border-border bg-surface-muted px-2 py-0.5 font-mono text-sm tracking-widest">{flow.user_code}</code>
            <button
              type="button"
              onClick={() => { void navigator.clipboard?.writeText(flow.user_code).catch(() => {}); setCopied(true); }}
              className="rounded border border-border px-1.5 py-0.5 text-[10px] font-medium text-foreground-muted hover:text-signal"
            >
              {copied ? "copied" : "copy"}
            </button>
          </span>
        </li>
      </ol>
      <p className="flex items-center gap-1.5 text-[11px] text-foreground-muted">
        <span className="h-2 w-2 animate-pulse rounded-full bg-signal" />
        {pending === "slow_down" ? "GitHub requested slower polling — waiting before checking again…" : pending === "authorization_pending" ? "Waiting for approval…" : "Checking sign-in status…"}
      </p>
      <button type="button" onClick={() => controller.current?.cancel()} className="text-xs text-foreground-muted">Cancel sign-in</button>
      {error && <p className="text-[11px] text-danger">{error}</p>}
    </div>
  );
}
