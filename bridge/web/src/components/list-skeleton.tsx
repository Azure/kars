import { Skeleton } from "@/components/ui";

/// A shared loading scaffold for the workspace list pages (missions, teams,
/// inbox, artifacts, skills). Shown via Next.js `loading.tsx` while the server
/// component fetches from the BFF — so a slow round-trip reads as "loading",
/// never a blank page that looks broken.
export function ListSkeleton({ rows = 4, title = "Loading…" }: { rows?: number; title?: string }) {
  return (
    <div className="space-y-6" aria-busy="true" aria-live="polite">
      <div>
        <Skeleton className="h-7 w-48" />
        <Skeleton className="mt-2 h-4 w-72" />
        <span className="sr-only">{title}</span>
      </div>
      <ul className="space-y-3">
        {Array.from({ length: rows }).map((_, i) => (
          <li key={i} className="rounded-xl border border-border bg-surface p-5">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0 flex-1">
                <Skeleton className="h-5 w-1/3" />
                <Skeleton className="mt-2 h-3.5 w-2/3" />
              </div>
              <Skeleton className="h-6 w-20 rounded-full" />
            </div>
            <div className="mt-4 flex gap-4">
              <Skeleton className="h-3.5 w-24" />
              <Skeleton className="h-3.5 w-24" />
              <Skeleton className="h-3.5 w-24" />
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}
