"use client";

// kars Bridge — workspace communication channels (agent-agnostic). Configured on
// the Connections tab alongside GitHub: wire Telegram / Slack / Discord / WhatsApp
// once for the whole WORKSPACE and every agent — mission or team — can report over
// them (the controller propagates the token into each run sandbox). SECURITY:
// tokens are write-only — typed into a password field, sent to the BFF (stored
// only in a K8s Secret), never shown back. The UI only knows which are enabled.

import { useCallback, useEffect, useState } from "react";
import { Icon } from "@/components/icon";

const CHANNELS: { id: string; label: string; glyph: "message"; token: string; hint: string; extra?: string }[] = [
  { id: "telegram", label: "Telegram", glyph: "message", token: "Bot token", hint: "From @BotFather. Add chat IDs below so agents can proactively DM you updates.", extra: "Allowed chat IDs (required for agents to send you updates)" },
  { id: "slack", label: "Slack", glyph: "message", token: "Bot OAuth token", hint: "xoxb-… — inbound conversation (agents reply to your DMs)." },
  { id: "discord", label: "Discord", glyph: "message", token: "Bot token", hint: "From the Discord developer portal — inbound conversation." },
  { id: "whatsapp", label: "WhatsApp", glyph: "message", token: "Enable", hint: "Type 'true' to enable pairing — inbound conversation." },
];

export function ConnectChannels({ ns }: { ns: string }) {
  const [enabled, setEnabled] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [token, setToken] = useState("");
  const [allowFrom, setAllowFrom] = useState("");
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const r = await fetch(`/api/namespaces/${ns}/channels`, { cache: "no-store" });
      const d = await r.json();
      setEnabled(d.enabled ?? []);
    } catch {
      setError("Couldn't load channels.");
      setEnabled([]);
    }
  }, [ns]);

  useEffect(() => {
    const timer = window.setTimeout(() => void load(), 0);
    return () => window.clearTimeout(timer);
  }, [load]);

  async function save(channel: string) {
    if (!token.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const r = await fetch(`/api/namespaces/${ns}/channels`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ channel, token: token.trim(), allow_from: allowFrom.trim() || undefined }),
      });
      if (!r.ok) {
        const b = await r.json().catch(() => null);
        throw new Error(b?.error?.message || "save failed");
      }
      setToken("");
      setAllowFrom("");
      setOpen(null);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "save failed");
    } finally {
      setBusy(false);
    }
  }

  async function disable(channel: string) {
    setBusy(true);
    setError(null);
    try {
      await fetch(`/api/namespaces/${ns}/channels/${channel}`, { method: "DELETE" });
      await load();
    } catch {
      setError("Disconnect failed.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <div className="grid gap-2 sm:grid-cols-2">
        {CHANNELS.map((ch) => {
          const on = enabled?.includes(ch.id) ?? false;
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
                </div>
                {on ? (
                  <button
                    type="button"
                    disabled={busy}
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
                      disabled={busy || !token.trim()}
                      onClick={() => save(ch.id)}
                      className="rounded-md bg-signal px-2.5 py-1 text-[11px] font-semibold text-signal-fg transition hover:opacity-90 disabled:opacity-50"
                    >
                      {busy ? "Saving…" : "Save"}
                    </button>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>
      {error && <p className="mt-2 text-xs text-rose-600">{error}</p>}
      <p className="mt-3 text-[11px] text-foreground-muted">
        Agent-agnostic: a channel wired here reaches every mission and team in this workspace — the
        controller injects the token into each run sandbox, and the agent&rsquo;s harness wires up the
        channel from it. No agent ever holds the token beyond its sandbox.
      </p>
    </div>
  );
}
