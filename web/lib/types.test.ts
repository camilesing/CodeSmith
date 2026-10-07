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
});
