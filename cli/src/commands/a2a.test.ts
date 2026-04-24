import { describe, expect, it } from "vitest";
import { a2aCommand } from "./a2a.js";

describe("a2aCommand", () => {
  it("registers the parent and two subcommands", () => {
    const cmd = a2aCommand();
    expect(cmd.name()).toBe("a2a");
    const sub = cmd.commands.map((c) => c.name()).sort();
    expect(sub).toEqual(["list-exposed", "schema"]);
  });

  it("list-exposed accepts -o json/yaml/table", async () => {
    const cmd = a2aCommand();
    // Smoke: the option is wired without throwing.
    const list = cmd.commands.find((c) => c.name() === "list-exposed")!;
    expect(list.options.find((o) => o.long === "--output")).toBeDefined();
    expect(list.options.find((o) => o.long === "--namespace")).toBeDefined();
  });

  it("schema subcommand exists with no required args", () => {
    const cmd = a2aCommand();
    const schema = cmd.commands.find((c) => c.name() === "schema")!;
    expect(schema.options.length).toBe(0);
  });
});
