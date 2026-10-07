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
 * Array items are validated for the fields the homepage renders directly
 * (`title`/`href` as strings): "valid JSON of the wrong shape" must degrade
 * to the fallback, not persist blank/garbage dispatch cards for 7 days.
 */
export function isDispatchPayload(v: unknown): v is Omit<CuratedDispatch, "generatedAt"> {
  if (typeof v !== "object" || v === null) return false;
  const d = v as CuratedDispatch;
  const wellFormedItem = (it: unknown): boolean =>
    typeof it === "object" &&
    it !== null &&
    typeof (it as { title?: unknown }).title === "string" &&
    typeof (it as { href?: unknown }).href === "string";
  // Optional zh sections: the zh homepage falls back to the English fields
  // only when they are ABSENT — a present-but-malformed zh array must degrade
  // to the fallback too, not crash the render (`highlights.slice(...)`).
  const wellFormedZhArray = (v: unknown) => Array.isArray(v) && v.every(wellFormedItem);
  return (
    typeof d.headline === "string" &&
    typeof d.summary === "string" &&
    Array.isArray(d.highlights) &&
    d.highlights.every(wellFormedItem) &&
    Array.isArray(d.movers) &&
    d.movers.every(wellFormedItem) &&
    (d.highlightsZh === undefined || wellFormedZhArray(d.highlightsZh)) &&
    (d.moversZh === undefined || wellFormedZhArray(d.moversZh))
  );
}
