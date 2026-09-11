// kars Bridge — a small, dependency-free line-icon set. Emoji render
// differently per-OS, can't inherit color/size, and read as amateur; these are
// consistent 1.5px-stroke glyphs that inherit `currentColor` and align to a
// grid. Add new icons here as needed rather than reaching for emoji.

import type { SVGProps } from "react";

export type IconName =
  | "loop"
  | "mirror"
  | "map"
  | "check-cycle"
  | "branch"
  | "eye"
  | "target"
  | "shield"
  | "brain"
  | "gear"
  | "globe"
  | "plug"
  | "database"
  | "wrench"
  | "stethoscope"
  | "scale"
  | "seal"
  | "coin"
  | "chart"
  | "note"
  | "layers"
  | "pencil"
  | "file"
  | "check"
  | "cross"
  | "warning"
  | "lock"
  | "download"
  | "refresh"
  | "bolt"
  | "compass"
  | "person"
  | "search"
  | "lightbulb"
  | "puzzle"
  | "box"
  | "link"
  | "message"
  | "flask"
  | "crown"
  | "terminal"
  | "handshake"
  | "chevron-down";

const PATHS: Record<IconName, React.ReactNode> = {
  // A closed feedback loop (observe → act → repeat).
  loop: (
    <>
      <path d="M3 8a5 5 0 0 1 9-3l1 1" />
      <path d="M13 3v3h-3" />
      <path d="M13 8a5 5 0 0 1-9 3l-1-1" />
      <path d="M3 13v-3h3" />
    </>
  ),
  // Reflection — a mirrored pair (reflect/critique).
  mirror: (
    <>
      <path d="M8 2v12" />
      <path d="M6 5 3 8l3 3" />
      <path d="m10 5 3 3-3 3" />
    </>
  ),
  // Plan/route — a waypointed path (plan-execute).
  map: (
    <>
      <path d="M4 12a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3Z" />
      <path d="M12 6a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3Z" />
      <path d="M5.5 10.5 10.5 5" strokeDasharray="1.5 1.5" />
    </>
  ),
  // Evaluate + iterate — a check inside a cycle.
  "check-cycle": (
    <>
      <path d="M13 8A5 5 0 1 1 11 4" />
      <path d="M6 8l1.5 1.5L11 5" />
    </>
  ),
  // Explore/branch — a forking tree.
  branch: (
    <>
      <path d="M5 2v5" />
      <path d="M5 7c0 2 3 2 3 4v3" />
      <path d="M5 7c0 2-3 2-3 4v0" />
      <circle cx="5" cy="2" r="1.4" />
      <circle cx="8" cy="14" r="1.4" />
    </>
  ),
  // Standing watch — an eye.
  eye: (
    <>
      <path d="M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8Z" />
      <circle cx="8" cy="8" r="1.8" />
    </>
  ),
  target: (
    <>
      <circle cx="8" cy="8" r="5.5" />
      <circle cx="8" cy="8" r="2.5" />
    </>
  ),
  shield: <path d="M8 1.5 3 3.5v4C3 11 5.5 13.5 8 14.5 10.5 13.5 13 11 13 7.5v-4Z" />,
  brain: (
    <>
      <path d="M6.5 2.5a2 2 0 0 0-2 2 2 2 0 0 0-1 3.5A2 2 0 0 0 5 11.5a2 2 0 0 0 1.5.5V2.5Z" />
      <path d="M9.5 2.5a2 2 0 0 1 2 2 2 2 0 0 1 1 3.5 2 2 0 0 1-1.5 3.5 2 2 0 0 1-1.5.5V2.5Z" />
    </>
  ),
  gear: (
    <>
      <circle cx="8" cy="8" r="2" />
      <path d="M8 1.5v2M8 12.5v2M14.5 8h-2M3.5 8h-2M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4M12.6 12.6l-1.4-1.4M4.8 4.8 3.4 3.4" />
    </>
  ),
  globe: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12" />
    </>
  ),
  plug: (
    <>
      <path d="M6 2v3M10 2v3" />
      <path d="M4.5 5h7v2a3.5 3.5 0 0 1-7 0Z" />
      <path d="M8 10.5V14" />
    </>
  ),
  database: (
    <>
      <ellipse cx="8" cy="4" rx="5" ry="2" />
      <path d="M3 4v8c0 1.1 2.2 2 5 2s5-.9 5-2V4" />
      <path d="M3 8c0 1.1 2.2 2 5 2s5-.9 5-2" />
    </>
  ),
  wrench: <path d="M11.5 2.5a3 3 0 0 0-3.9 3.9l-5 5a1.5 1.5 0 0 0 2 2l5-5a3 3 0 0 0 3.9-3.9L11 4.5 9.5 4 9 2.5Z" />,
  stethoscope: (
    <>
      <path d="M4 2v3a3 3 0 0 0 6 0V2" />
      <path d="M7 8v1.5a3.5 3.5 0 0 0 7 0V8" />
      <circle cx="12.5" cy="6.5" r="1.2" />
    </>
  ),
  scale: (
    <>
      <path d="M8 2v11M4 13h8M3 5l5-1 5 1" />
      <path d="M3 5 1.5 8.5a2 2 0 0 0 3 0Z" />
      <path d="M13 5l-1.5 3.5a2 2 0 0 0 3 0Z" />
    </>
  ),
  seal: (
    <>
      <circle cx="8" cy="6.5" r="4" />
      <path d="M6 10l-1 4 3-1.5L11 14l-1-4" />
    </>
  ),
  coin: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 5v6M6.3 6.2h2.4a1.3 1.3 0 0 1 0 2.6H6.3M6.3 8.8h2.6" />
    </>
  ),
  chart: (
    <>
      <path d="M2 2v12h12" />
      <path d="M5 10l2.5-3 2 2L13 4" />
    </>
  ),
  note: (
    <>
      <path d="M4 2h6l3 3v9H4Z" />
      <path d="M10 2v3h3M6 8h5M6 11h5" />
    </>
  ),
  layers: (
    <>
      <path d="M8 2 2 5l6 3 6-3Z" />
      <path d="M2 8.5 8 11.5 14 8.5M2 11.5 8 14.5 14 11.5" />
    </>
  ),
  pencil: (
    <>
      <path d="M10.5 2.5 13.5 5.5 5 14H2v-3Z" />
    </>
  ),
  file: (
    <>
      <path d="M4 1.5h5.5L12 4v10.5H4Z" />
      <path d="M9.5 1.5v3H12" />
    </>
  ),
  check: <path d="M2.5 8.5 6 12l7.5-8" />,
  cross: <path d="M3 3l10 10M13 3 3 13" />,
  warning: (
    <>
      <path d="M8 1.5 14.5 13H1.5Z" />
      <path d="M8 6.5v3M8 11.5v.01" />
    </>
  ),
  lock: (
    <>
      <path d="M4 7V4.5a4 4 0 0 1 8 0V7" />
      <path d="M2.5 7h11v7h-11Z" />
    </>
  ),
  download: (
    <>
      <path d="M8 1.5v8M5 6.5 8 9.5l3-3" />
      <path d="M2.5 12.5v2h11v-2" />
    </>
  ),
  refresh: (
    <>
      <path d="M2.5 8a5.5 5.5 0 0 1 9.5-3.8l1 1" />
      <path d="M13 2.5v3h-3" />
      <path d="M13.5 8a5.5 5.5 0 0 1-9.5 3.8l-1-1" />
      <path d="M3 13.5v-3h3" />
    </>
  ),
  bolt: <path d="M8.5 1.5 3 9h4l-.5 5.5L13 7H9Z" />,
  compass: (
    <>
      <circle cx="8" cy="8" r="6.5" />
      <path d="M10.5 5.5 9 9l-3.5 1.5L7 7Z" />
    </>
  ),
  person: (
    <>
      <circle cx="8" cy="5" r="2.5" />
      <path d="M2.5 14a5.5 5.5 0 0 1 11 0" />
    </>
  ),
  search: (
    <>
      <circle cx="7" cy="7" r="4.5" />
      <path d="M10.2 10.2 14 14" />
    </>
  ),
  lightbulb: (
    <>
      <path d="M8 1.5a4.5 4.5 0 0 0-2.5 8.25V11.5h5V9.75A4.5 4.5 0 0 0 8 1.5Z" />
      <path d="M6 13.5h4M6.5 15h3" />
    </>
  ),
  puzzle: (
    <>
      <path d="M4 4h3V2.5a1.2 1.2 0 1 1 2.4 0V4H12v3.4a1.2 1.2 0 1 0 0 2.4V13H8.6a1.2 1.2 0 1 0-2.4 0H4V9.6a1.2 1.2 0 1 1 0-2.4Z" />
    </>
  ),
  box: (
    <>
      <path d="M8 1.5 14 4.5v7L8 14.5 2 11.5v-7Z" />
      <path d="M2 4.5 8 7.5v7M14 4.5 8 7.5" />
    </>
  ),
  link: (
    <>
      <path d="M6.5 9.5 9.5 6.5" />
      <path d="M7 4.5 8.7 2.8a2.6 2.6 0 0 1 3.7 3.7L10.6 8.2" />
      <path d="M9 11.5 7.3 13.2a2.6 2.6 0 0 1-3.7-3.7L5.4 7.8" />
    </>
  ),
  message: (
    <>
      <path d="M2 3h12v8H6l-3 3v-3H2Z" />
    </>
  ),
  flask: (
    <>
      <path d="M6.5 2h3M7 2v4l-4 7a1 1 0 0 0 .9 1.5h8.2A1 1 0 0 0 13 13l-4-7V2" />
      <path d="M5 10.5h6" />
    </>
  ),
  crown: <path d="M2.5 12.5 1.5 5 5.5 8 8 3.5 10.5 8l4-3-1 7.5Z" />,
  terminal: (
    <>
      <path d="M2 2.5h12v11H2Z" />
      <path d="M4.5 6 7 8.5 4.5 11M8.5 11h3" />
    </>
  ),
  handshake: (
    <>
      <path d="M1.5 8.5 4 6l2.5 2-1 1.5" />
      <path d="M14.5 8.5 12 6l-2.5 2 1 1.5" />
      <path d="M6.5 8l1.5 1.5L9.5 8" />
    </>
  ),
  "chevron-down": <path d="M3.5 6 8 10.5 12.5 6" />,
};

export function Icon({
  name,
  size = 16,
  ...props
}: { name: IconName; size?: number } & Omit<SVGProps<SVGSVGElement>, "name">) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      {...props}
    >
      {PATHS[name]}
    </svg>
  );
}
