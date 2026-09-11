"use client";

// kars Bridge — team communication channels. Part of a standing team's envelope:
// wire Telegram / Slack / Discord / WhatsApp so the team reports its progress and
// deliverables to the operator. SECURITY: tokens are write-only — typed into a
// password field, sent to the BFF (stored only in a K8s Secret), and never shown
// back. The UI knows which channels are enabled plus whether the current team
// route has retained qualification evidence for each adapter.

import { useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { setTeamChannel, deleteTeamChannel } from "./channel-actions";
import { Icon } from "@/components/icon";
import type { TeamChannelStatus } from "@/lib/types";

const CHANNELS: { id: string; label: string; glyph: "message"; token: string; hint: string; extra?: string }[] = [
  { id: "telegram", label: "Telegram", glyph: "message", token: "Bot token", hint: "From @BotFather", extra: "Allowed user IDs (comma-separated)" },
  { id: "slack", label: "Slack", glyph: "message", token: "Bot OAuth token", hint: "xoxb-…" },
  { id: "discord", label: "Discord", glyph: "message", token: "Bot token", hint: "From the Discord developer portal" },
  { id: "whatsapp", label: "WhatsApp", glyph: "message", token: "Enable", hint: "Type 'true' to enable pairing" },
];

export function TeamChannels({
  team,
  enabled,
  statuses = [],
}: {
  team: string;
  enabled: string[];
  statuses?: TeamChannelStatus[];
}) {
  const router = useRouter();
  const [pending, startTransition] = useTransition();
  const [open, setOpen] = useState<string | null>(null);
  const [token, setToken] = useState("");
  const [allowFrom, setAllowFrom] = useState("");
  const [error, setError] = useState<string | null>(null);
  const statusByChannel = new Map(statuses.map((status) => [status.channel, status] as const));

  function save(channel: string) {
    if (!token.trim()) return;
    setError(null);
    startTransition(async () => {
      const res = await setTeamChannel(team, channel, token.trim(), allowFrom.trim() || undefined);
      if (res.error) {
        setError(res.error);
        return;
      }
      setToken("");
      setAllowFrom("");
      setOpen(null);
      router.refresh();
    });
  }

  function disable(channel: string) {
    setError(null);
    startTransition(async () => {
      const res = await deleteTeamChannel(team, channel);
      if (res?.error) {
        setError(res.error);
        return;
      }
      router.refresh();
    });
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <h2 className="text-sm font-semibold">Communication channels</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">
        Part of the team&apos;s envelope. Wire a channel and the team reports milestones and its
        deliverables to you there. Tokens are stored encrypted-at-rest as a Kubernetes secret and are
        never shown again.
      </p>

      <div className="mt-4 grid gap-2 sm:grid-cols-2">
        {CHANNELS.map((ch) => {
          const status = statusByChannel.get(ch.id);
          const on = status?.enabled ?? enabled.includes(ch.id);
          const isOpen = open === ch.id;
          return (
            <div key={ch.id} className="rounded-lg border border-border p-3">
              <div className="flex items-center justify-between gap-2">
                <div className="flex items-center gap-2">
                  <span aria-hidden className="text-base leading-none"><Icon name={ch.glyph} size={16} /></span>
                  <span className="text-sm font-medium">{ch.label}</span>
                  {on && (
                    <span className="rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-600">
                      connected
                    </span>
                  )}
                  {status?.qualified === true && (
                    <span className="rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2 py-0.5 text-[10px] font-medium text-emerald-600">
                      qualified
                    </span>
                  )}
                  {status?.qualified === false && (
                    <span className="rounded-full border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-[10px] font-medium text-amber-700">
                      unqualified
                    </span>
                  )}
                </div>
                {on ? (
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => disable(ch.id)}
                    className="rounded-md border border-border px-2 py-1 text-[11px] text-foreground-muted transition hover:text-rose-600 disabled:opacity-50"
                  >
                    Disconnect
                  </button>
                ) : (
                  <button
                    type="button"
                    onClick={() => {
                      setOpen(isOpen ? null : ch.id);
                      setToken("");
                      setAllowFrom("");
                      setError(null);
                    }}
                    className="rounded-md border border-border px-2 py-1 text-[11px] font-medium transition hover:bg-surface-muted"
                  >
                    {isOpen ? "Cancel" : "Connect"}
                  </button>
                )}
              </div>
              {status?.detail && (
                <p className="mt-2 text-[11px] text-foreground-muted">{status.detail}</p>
              )}
              {isOpen && !on && (
                <div className="mt-2 space-y-2">
                  <input
                    type="password"
                    autoComplete="off"
                    value={token}
                    onChange={(e) => setToken(e.target.value)}
                    placeholder={ch.token}
                    className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs outline-none focus:border-signal"
                  />
                  {ch.extra && (
                    <input
                      type="text"
                      value={allowFrom}
                      onChange={(e) => setAllowFrom(e.target.value)}
                      placeholder={ch.extra}
                      className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs outline-none focus:border-signal"
                    />
                  )}
                  <div className="flex items-center justify-between">
                    <span className="text-[10px] text-foreground-muted">{ch.hint}</span>
                    <button
                      type="button"
                      disabled={pending || !token.trim()}
                      onClick={() => save(ch.id)}
                      className="rounded-md bg-signal px-2.5 py-1 text-[11px] font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
                    >
                      {pending ? "Saving…" : "Save"}
                    </button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
      {error && <p className="mt-2 text-xs text-rose-600">{error}</p>}
    </section>
  );
}
