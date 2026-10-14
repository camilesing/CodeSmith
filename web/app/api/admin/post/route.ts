import { NextResponse } from "next/server";
import { getAgentEnv, getDraft, deleteDraft, validateSession, type CommunityAgentEnv } from "@/lib/community-agent";
import { GITHUB_REPO } from "@/lib/constants";

export const dynamic = "force-dynamic";

async function checkAuth(req: Request, env: CommunityAgentEnv): Promise<{ ok: boolean; status?: number; error?: string }> {
  if (!env.MAINTAINER_TOKEN) {
    return { ok: false, status: 503, error: "MAINTAINER_TOKEN not configured" };
  }

  const cookieHeader = req.headers.get("cookie") ?? "";
  let sid: string | undefined;
  for (const c of cookieHeader.split(";")) {
    const [name, ...rest] = c.trim().split("=");
    if (name === "mt_sid") {
      sid = rest.join("=");
      break;
    }
  }

  if (!sid || !(await validateSession(env.CURATED_KV, sid))) {
    return { ok: false, status: 401, error: "unauthorized" };
  }
  return { ok: true };
}

const ALLOWED_ACTIONS = new Set(["post", "discard"]);
const ALLOWED_ORIGINS = new Set(["https://codesmith.net", "https://www.codesmith.net"]);
const MAX_BODY_BYTES = 65_536;
// draft:<type>:<id> exactly as draftKey() produces it — CURATED_KV is a
// shared namespace (it also holds session:admin:* and usage:* keys), so a
// free-form key must not reach getDraft/deleteDraft.
const DRAFT_KEY_RE = /^draft:(triage|pr-review|stale|dupes|digest):[A-Za-z0-9_-]{1,64}$/;

export async function POST(req: Request) {
  const env = await getAgentEnv();

  // CSRF: this is a state-changing admin endpoint riding a cookie. A
  // missing Origin is only ever a non-browser client (browsers always
  // send it on POST), and Sec-Fetch-Site must agree the request is
  // same-origin.
  const origin = req.headers.get("origin");
  const fetchSite = req.headers.get("sec-fetch-site");
  if (!origin || !ALLOWED_ORIGINS.has(origin) || (fetchSite && fetchSite !== "same-origin")) {
    return NextResponse.json({ error: "forbidden origin" }, { status: 403 });
  }

  const auth = await checkAuth(req, env);
  if (!auth.ok) {
    return NextResponse.json(
      { error: auth.error ?? "unauthorized" },
      { status: auth.status ?? 401, headers: { "Cache-Control": "no-store" } }
    );
  }

  const contentLength = Number(req.headers.get("content-length") ?? "0");
  if (contentLength > MAX_BODY_BYTES) {
    return NextResponse.json({ error: "payload too large" }, { status: 413 });
  }

  const body = await req.json() as { action: string; draftKey: string; editedBody?: string; lang?: "en" | "zh" };
  const { action, draftKey, editedBody, lang } = body;

  if (!ALLOWED_ACTIONS.has(action)) {
    return NextResponse.json({ error: "unknown action" }, { status: 400 });
  }
  if (typeof draftKey !== "string" || !DRAFT_KEY_RE.test(draftKey)) {
    return NextResponse.json({ error: "missing or invalid draftKey" }, { status: 400 });
  }
  if (editedBody !== undefined && (typeof editedBody !== "string" || editedBody.length > MAX_BODY_BYTES)) {
    return NextResponse.json({ error: "editedBody too long" }, { status: 413 });
  }
  if (lang !== undefined && lang !== "en" && lang !== "zh") {
    return NextResponse.json({ error: "invalid lang" }, { status: 400 });
  }

  const draft = await getDraft(env.CURATED_KV, draftKey);
  if (!draft) {
    return NextResponse.json({ error: "draft not found" }, { status: 404 });
  }

  if (action === "discard") {
    await deleteDraft(env.CURATED_KV, draftKey);
    return NextResponse.json({ ok: true, action: "discarded" });
  }

  if (action === "post") {
    if (!env.MAINTAINER_GITHUB_PAT) {
      return NextResponse.json({ error: "MAINTAINER_GITHUB_PAT not configured" }, { status: 500 });
    }

    const commentBody = editedBody ?? (lang === "zh" ? draft.bodyZh : draft.bodyEn);

    if (draft.type === "digest") {
      return NextResponse.json({ ok: true, action: "digest-skipped", note: "Digest pages are not posted as comments" });
    }

    if (!draft.targetNumber) {
      return NextResponse.json({ error: "no target number" }, { status: 400 });
    }

    // Idempotency: `posted` only flips after a successful GitHub fetch,
    // so a client retry after a timeout (or a KV write failure after a
    // successful post) used to re-publish the same comment — a visible
    // duplicate on the public issue.
    if (action === "post" && draft.posted) {
      return NextResponse.json({ ok: true, action: "already-posted" });
    }

    const repo = env.GITHUB_REPO ?? GITHUB_REPO;
    const commentUrl = `https://api.github.com/repos/${repo}/issues/${draft.targetNumber}/comments`;

    const ghRes = await fetch(commentUrl, {
      method: "POST",
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${env.MAINTAINER_GITHUB_PAT}`,
        "X-GitHub-Api-Version": "2022-11-28",
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ body: commentBody }),
    });

    if (!ghRes.ok) {
      // Log a truncated body server-side only: GitHub error bodies can
      // carry internal request IDs, rate-limit/abuse metadata, or org and
      // repo details that must not be proxied to the browser.
      const text = (await ghRes.text()).slice(0, 512);
      console.error(`GitHub comment post failed (${draftKey})`, ghRes.status, text);
      return NextResponse.json(
        { error: `GitHub API error (${ghRes.status})` },
        { status: 502 },
      );
    }

    // Mark as posted
    draft.posted = true;
    await env.CURATED_KV?.put(draftKey, JSON.stringify(draft), { expirationTtl: 60 * 60 * 24 * 7 });

    return NextResponse.json({ ok: true, action: "posted" });
  }

  // ALLOWED_ACTIONS guard above means this is unreachable.
  return NextResponse.json({ error: "unknown action" }, { status: 400 });
}
