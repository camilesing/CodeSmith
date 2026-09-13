import type { FeedItem } from "@/lib/types";
import { relativeTime } from "@/lib/github";

function TickerItem({ item, duplicated = false }: { item: FeedItem; duplicated?: boolean }) {
  return (
    // The duplicated half exists only for the seamless-loop animation;
    // aria-hidden keeps screen readers from announcing every item twice.
    <span aria-hidden={duplicated || undefined} className="inline-flex items-center gap-2">
      <span className="text-indigo uppercase tracking-wider">{item.kind === "pull" ? "PR" : "ISS"}</span>
      <span className="tabular text-ink-mute">#{item.number}</span>
      <span className="text-ink">{item.title.slice(0, 78)}{item.title.length > 78 ? "…" : ""}</span>
      <span className="text-ink-mute tabular">· {relativeTime(item.updatedAt)}</span>
      <span className="text-paper-line">◆</span>
    </span>
  );
}

export function Ticker({ items }: { items: FeedItem[] }) {
  if (!items.length) return null;
  // Two sequential copies for a seamless loop; keys stay unique via the loop index.
  return (
    <div className="hairline-t hairline-b bg-paper-deep overflow-hidden">
      <div className="mx-auto max-w-[1400px] flex items-stretch">
        <div className="bg-ink text-paper px-4 py-2 flex items-center shrink-0 gap-2">
          <span className="w-1.5 h-1.5 bg-indigo rounded-full inline-block animate-pulse" />
          <span className="font-cjk text-sm font-semibold tracking-wider">实 时</span>
          <span className="font-mono text-[0.7rem] uppercase tracking-widest text-paper-deep">LIVE</span>
        </div>
        <div className="flex-1 overflow-hidden relative">
          <div className="ticker-track py-2 font-mono text-[0.78rem]">
            {items.map((item, i) => (
              <TickerItem key={`${item.url}-${i}`} item={item} />
            ))}
            {items.map((item, i) => (
              <TickerItem key={`${item.url}-${items.length + i}`} item={item} duplicated />
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
