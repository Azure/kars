import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const dockerfile = readFileSync(new URL("../../bff/Dockerfile", import.meta.url), "utf8");
const ignored = readFileSync(new URL("../../bff/.dockerignore", import.meta.url), "utf8");

describe("Private BFF image build contract", () => {
  it("compiles real sources once with a locked, fail-closed build", () => {
    const builder = (dockerfile.split(/^FROM .* AS runtime$/m)[0] ?? "")
      .split("\n")
      .filter((line) => !line.trimStart().startsWith("#"))
      .join("\n");
    expect(builder.match(/cargo\s+build\b/g)).toHaveLength(1);
    expect(builder).toMatch(/RUN cargo build --release --locked && strip target\/release\/kars-bridge-bff/);
    expect(builder).not.toMatch(/\|\|\s*true/);
    expect(builder).not.toContain("fn main() {}");
    expect(builder.indexOf("COPY . .")).toBeGreaterThanOrEqual(0);
    expect(builder.indexOf("COPY . .")).toBeLessThan(builder.indexOf("cargo build"));
  });

  it("uses the resulting non-root binary, not host build artifacts", () => {
    expect(ignored.split(/\r?\n/)).toContain("target/");
    expect(dockerfile).toContain(
      "COPY --from=build /src/target/release/kars-bridge-bff /usr/local/bin/kars-bridge-bff",
    );
    expect(dockerfile).toMatch(/^USER 10001$/m);
    expect(dockerfile).toContain('ENTRYPOINT ["/usr/local/bin/kars-bridge-bff"]');
  });
});
