"use client";

// A real Rubik's cube rendered in CSS 3D — six faces, each a 3×3 grid of
// classic-colored stickers on a dark plastic body, tumbling smoothly. Pure
// presentation for the orchestration flow; no external deps. Deterministic
// sticker colors (the solved cube) so it reads instantly as a Rubik's cube.

const FACE_COLORS: Record<string, string> = {
  U: "#f8fafc", // up — white
  D: "#facc15", // down — yellow
  F: "#22c55e", // front — green
  B: "#3b82f6", // back — blue
  R: "#ef4444", // right — red
  L: "#f97316", // left — orange
};

// Face transform for a cube of edge `s` (px): position + orient each face.
function faceTransform(face: string, s: number): string {
  const h = s / 2;
  switch (face) {
    case "F": return `translateZ(${h}px)`;
    case "B": return `rotateY(180deg) translateZ(${h}px)`;
    case "R": return `rotateY(90deg) translateZ(${h}px)`;
    case "L": return `rotateY(-90deg) translateZ(${h}px)`;
    case "U": return `rotateX(90deg) translateZ(${h}px)`;
    case "D": return `rotateX(-90deg) translateZ(${h}px)`;
    default: return "";
  }
}

export function RubiksCube({ size = 104, settled = false, assembling = false }: { size?: number; settled?: boolean; assembling?: boolean }) {
  const faces = ["F", "B", "R", "L", "U", "D"];
  return (
    <div className="kb-rubik-scene" style={{ width: size, height: size }}>
      <div className={`kb-rubik ${settled ? "kb-rubik-settle" : ""} ${assembling ? "kb-rubik-assembling" : ""}`} style={{ width: size, height: size }}>
        {faces.map((f, fi) => (
          <div key={f} className="kb-rubik-face" style={{ width: size, height: size, transform: faceTransform(f, size) }}>
            {Array.from({ length: 9 }).map((_, i) => (
              <span
                key={i}
                className="kb-rubik-sticker"
                style={{
                  background: FACE_COLORS[f],
                  // Self-assembly: stickers cascade in (staggered by position)
                  // while the package is being computed, so the cube visibly
                  // builds itself during the loading calculation.
                  ...(assembling ? { animationDelay: `${(fi * 9 + i) * 26}ms` } : {}),
                }}
              />
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
