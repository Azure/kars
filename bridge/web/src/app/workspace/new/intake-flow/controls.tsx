import type * as React from "react";
import { useState } from "react";
import { useFormStatus } from "react-dom";
import type { BlueprintEgress } from "@/lib/types";

export function CheckMark({ status }: { status: "pass" | "fail" | "warn" }) {
  const map = {
    pass: { c: "text-ok", s: "✓" },
    warn: { c: "text-warning", s: "!" },
    fail: { c: "text-danger", s: "✕" },
  } as const;
  const m = map[status];
  return (
    <span className={`mt-0.5 inline-flex h-4 w-4 shrink-0 items-center justify-center rounded-full border text-[10px] font-bold ${m.c}`} aria-hidden>
      {m.s}
    </span>
  );
}

export function CreateButton({
  disabled,
  launch,
  needsValidation,
}: {
  disabled: boolean;
  launch: boolean;
  needsValidation: boolean;
}) {
  const { pending } = useFormStatus();
  const label = pending
    ? "Creating…"
    : needsValidation
      ? "Validate to launch"
      : launch
        ? "Create & launch"
        : "Create draft";
  return (
    <button
      type="submit"
      disabled={pending || disabled}
      className="rounded-lg bg-signal px-5 py-2.5 text-sm font-semibold text-signal-fg shadow-sm transition hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal disabled:opacity-50"
    >
      {label}
    </button>
  );
}

export function EgressEditor({
  egress,
  onChange,
}: {
  egress: BlueprintEgress[];
  onChange: (e: BlueprintEgress[]) => void;
}) {
  const [host, setHost] = useState("");
  const [port, setPort] = useState("443");
  const [err, setErr] = useState<string | null>(null);

  // A permissive hostname / IPv4 check — rejects schemes, paths, spaces, and
  // obvious junk so a bad allowlist entry can't silently reach the controller.
  const HOST_RE = /^(?:\*\.)?(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$|^(?:\d{1,3}\.){3}\d{1,3}$|^localhost$/;

  function add() {
    setErr(null);
    const h = host.trim().toLowerCase();
    if (!h) return;
    if (h.includes("/") || h.includes(":") || h.includes(" ")) {
      setErr("Enter a bare hostname (no scheme, port, or path) — set the port separately.");
      return;
    }
    if (!HOST_RE.test(h)) {
      setErr("That doesn't look like a valid hostname or IP.");
      return;
    }
    let p: number | null = null;
    if (port.trim() !== "") {
      const n = Number(port);
      if (!Number.isInteger(n) || n < 1 || n > 65535) {
        setErr("Port must be a whole number between 1 and 65535.");
        return;
      }
      p = n;
    }
    if (egress.some((e) => e.host === h && e.port === p)) {
      setErr("That host:port is already in the allowlist.");
      return;
    }
    onChange([...egress, { host: h, port: p }]);
    setHost("");
    setPort("443");
  }

  return (
    <div className="space-y-2">
      {egress.length > 0 && (
        <ul className="space-y-1.5">
          {egress.map((e, i) => (
            <li
              key={`${e.host}:${e.port ?? ""}:${i}`}
              className="flex items-center justify-between rounded-lg bg-surface-muted px-3 py-1.5 text-sm"
            >
              <span className="font-mono text-xs">
                {e.host}
                {e.port ? `:${e.port}` : ""}
              </span>
              <button
                type="button"
                onClick={() => onChange(egress.filter((_, j) => j !== i))}
                className="text-xs text-foreground-muted hover:text-danger"
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex items-center gap-2">
        <input
          value={host}
          onChange={(e) => setHost(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
          placeholder="host, e.g. api.github.com"
          className="flex-1 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
        <input
          value={port}
          onChange={(e) => setPort(e.target.value)}
          placeholder="443"
          inputMode="numeric"
          className="w-20 rounded-lg border border-border bg-surface px-3 py-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        />
        <button
          type="button"
          onClick={add}
          className="rounded-lg border border-border bg-surface px-3 py-2 text-sm font-medium transition hover:bg-surface-muted"
        >
          Add
        </button>
      </div>
      {err && <p className="text-xs text-danger">{err}</p>}
    </div>
  );
}

export function PackageSection({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-2xl border border-border bg-surface p-5 shadow-sm">
      <h2 className="text-sm font-semibold">{title}</h2>
      <p className="mt-0.5 text-xs text-foreground-muted">{subtitle}</p>
      <div className="mt-3">{children}</div>
    </section>
  );
}
