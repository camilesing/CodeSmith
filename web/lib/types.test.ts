import { describe, expect, it } from "vitest";
import { isDispatchPayload } from "./types";

const validPayload = {
  headline: "headline",
  summary: "summary",
  highlights: [{ title: "t", href: "https://github.com/x", tag: "tag", blurb: "b" }],
  movers: [{ number: 1, title: "t", href: "https://github.com/y", reason: "r" }],
};

describe("isDispatchPayload", () => {
  it("accepts a well-formed payload", () => {
    expect(isDispatchPayload(validPayload)).toBe(true);
  });

  it("rejects non-objects and missing scalar fields", () => {
    expect(isDispatchPayload(null)).toBe(false);
    expect(isDispatchPayload("n/a")).toBe(false);
    expect(isDispatchPayload({ ...validPayload, headline: 3 })).toBe(false);
    expect(isDispatchPayload({ ...validPayload, movers: "no" })).toBe(false);
  });

  it("rejects malformed array items — valid JSON of the wrong shape must degrade to the fallback", () => {
    expect(isDispatchPayload({ ...validPayload, highlights: ["n/a"] })).toBe(false);
    expect(
      isDispatchPayload({
        ...validPayload,
        movers: [{ href: "https://github.com/y" }], // missing title
      })
    ).toBe(false);
  });

  it("rejects items missing the fields the homepage renders (tag/blurb) or the write side persists (number/reason)", () => {
    // highlight without tag — the en/zh homepage renders h.tag directly.
    expect(
      isDispatchPayload({
        ...validPayload,
        highlights: [{ title: "t", href: "https://github.com/x", blurb: "b" }],
      })
    ).toBe(false);
    // mover with a mistyped number — persisted to KV even though unrendered.
    expect(
      isDispatchPayload({
        ...validPayload,
        movers: [{ number: "1", title: "t", href: "https://github.com/y", reason: "r" }],
      })
    ).toBe(false);
  });

  it("accepts well-formed optional zh sections", () => {
    expect(
      isDispatchPayload({
        ...validPayload,
        highlightsZh: [{ title: "t", href: "https://github.com/z", tag: "tag", blurb: "b" }],
        moversZh: [{ number: 1, title: "t", href: "https://github.com/z", reason: "r" }],
      })
    ).toBe(true);
  });

  it("rejects present-but-malformed zh sections (zh homepage renders them unguarded)", () => {
    expect(isDispatchPayload({ ...validPayload, highlightsZh: "n/a" })).toBe(false);
    expect(isDispatchPayload({ ...validPayload, highlightsZh: [{ title: "t" }] })).toBe(false);
    expect(isDispatchPayload({ ...validPayload, moversZh: ["n/a"] })).toBe(false);
  });

  it("rejects non-https hrefs — the write side's allowlist, defended in depth on the read side", () => {
    // A payload from a manual KV edit or a future writer that bypasses
    // curate must degrade to the fallback, not render into homepage anchors.
    expect(
      isDispatchPayload({
        ...validPayload,
        highlights: [{ title: "t", href: "javascript:alert(1)", tag: "tag", blurb: "b" }],
      })
    ).toBe(false);
    expect(
      isDispatchPayload({
        ...validPayload,
        movers: [{ number: 1, title: "t", href: "/relative/path", reason: "r" }],
      })
    ).toBe(false);
    expect(
      isDispatchPayload({
        ...validPayload,
        movers: [{ number: 1, title: "t", href: "http://insecure.example/y", reason: "r" }],
      })
    ).toBe(false);
  });

  it("rejects non-string optional zh scalars — truthiness-only render would pass a number or crash on an object", () => {
    expect(isDispatchPayload({ ...validPayload, headlineZh: 42 })).toBe(false);
    expect(isDispatchPayload({ ...validPayload, summaryZh: { text: "…" } })).toBe(false);
  });
});
