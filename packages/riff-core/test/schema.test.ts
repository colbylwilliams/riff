import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { validate } from "../src/schema.ts";

describe("schema validation", () => {
  it("measures string length in code points, as JSON Schema specifies", () => {
    // 31 emoji: 31 code points, but 62 UTF-16 units. Counting units would reject a valid value,
    // and counting grapheme clusters would disagree with the other binding.
    const value = "🎧".repeat(31);
    const result = validate({ type: "string", maxLength: 60 }, value);

    assert.equal(value.length, 62, "the naive count is over the limit");
    assert.ok(result.valid, result.errors.join("; "));
  });

  it("still enforces the limit on ordinary text", () => {
    assert.equal(validate({ type: "string", maxLength: 3 }, "abcd").valid, false);
    assert.equal(validate({ type: "string", minLength: 2 }, "a").valid, false);
  });

  it("applies declared defaults", () => {
    const result = validate<{ keep: boolean }>(
      { type: "object", properties: { keep: { type: "boolean", default: false } } },
      {},
    );
    assert.equal(result.value.keep, false);
  });

  it("rejects properties the schema does not declare", () => {
    const result = validate({ type: "object", additionalProperties: false, properties: {} }, { nope: 1 });
    assert.match(result.errors.join(" "), /unexpected property "nope"/);
  });
});
