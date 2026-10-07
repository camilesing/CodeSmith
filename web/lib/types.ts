export type FeedKind = "issue" | "pull" | "release" | "discussion";

export interface FeedItem {
  kind: FeedKind;
  number: number;
  title: string;
  url: string;
  state: "open" | "closed" | "merged" | "draft" | "published";
  author: string;
  authorAvatar: string;
  createdAt: string; // ISO
  updatedAt: string; // ISO
  comments: number;
  labels: { name: string; color: string }[];
  body?: string;
}

export interface RepoStats {
  stars: number;
  forks: number;
  openIssues: number;
  openPulls: number;
  contributors: number;
  latestRelease?: { tag: string; publishedAt: string; url: string };
  fetchedAt: string;
}

export interface CuratedDispatch {
  generatedAt: string;
  /** English — always present (backward compat). */
  headline: string;
  summary: string;
  highlights: { title: string; href: string; tag: string; blurb: string }[];
  movers: { number: number; title: string; href: string; reason: string }[];
  /** zh-CN — populated by cron curate since ~May 2026. Falls back to English fields when absent. */
  headlineZh?: string;
  summaryZh?: string;
  highlightsZh?: { title: string; href: string; tag: string; blurb: string }[];
  moversZh?: { number: number; title: string; href: string; reason: string }[];
}

/**
 * Shape guard for a dispatch payload, shared by the write side (llm.ts curate)
 * and the read side (kv.ts getDispatch) of the same KV key — kept in one place
 * so the two halves cannot drift and silently wedge the homepage on the static
 * fallback. `generatedAt` is checked by neither side (the write side stamps it
 * itself; the read side tolerates its absence and stamps a default).
 *
 * Item fields are validated per list shape — everything the homepage renders
 * directly, plus what the write side persists (movers' `number`/`reason` are
 * not rendered today but stay checked so a malformed payload cannot squat in
 * KV for 7 days): "valid JSON of the wrong shape" must degrade to the
 * fallback, not persist blank/garbage dispatch cards.
 */
export function isDispatchPayload(v: unknown): v is Omit<CuratedDispatch, "generatedAt"> {
  if (typeof v !== "object" || v === null) return false;
  const d = v as CuratedDispatch;
  const hasStr = (it: unknown, key: string): boolean =>
    typeof (it as { [k: string]: unknown })[key] === "string";
  const wellFormedBase = (it: unknown): boolean =>
    typeof it === "object" && it !== null && hasStr(it, "title") && hasStr(it, "href");
  const wellFormedHighlight = (it: unknown): boolean =>
    wellFormedBase(it) && hasStr(it, "tag") && hasStr(it, "blurb");
  const wellFormedMover = (it: unknown): boolean =>
    wellFormedBase(it) &&
    typeof (it as { number?: unknown }).number === "number" &&
    hasStr(it, "reason");
  const wellFormedList = (v: unknown, pred: (it: unknown) => boolean): boolean =>
    Array.isArray(v) && v.every(pred);
  return (
    typeof d.headline === "string" &&
    typeof d.summary === "string" &&
    wellFormedList(d.highlights, wellFormedHighlight) &&
    wellFormedList(d.movers, wellFormedMover) &&
    // Optional zh fields: the zh homepage falls back to the English fields
    // only when they are ABSENT — a present-but-malformed zh value (scalar
    // or array) must degrade to the fallback too: a number renders raw, an
    // object throws "Objects are not valid as a React child" and crashes
    // the zh homepage until the next curate run overwrites the key.
    (d.headlineZh === undefined || typeof d.headlineZh === "string") &&
    (d.summaryZh === undefined || typeof d.summaryZh === "string") &&
    (d.highlightsZh === undefined || wellFormedList(d.highlightsZh, wellFormedHighlight)) &&
    (d.moversZh === undefined || wellFormedList(d.moversZh, wellFormedMover))
  );
}
