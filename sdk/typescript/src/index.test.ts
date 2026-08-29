import { describe, expect, test } from "bun:test";

import { decodeMessage, encodeCommand } from "./index";

describe("Ditto JSON Lines protocol", () => {
  test("encodes one command per line", () => {
    expect(encodeCommand({ type: "ping" })).toBe('{"type":"ping"}\n');
  });

  test("rejects messages without a type discriminator", () => {
    expect(() => decodeMessage("{}")).toThrow(TypeError);
  });
});
