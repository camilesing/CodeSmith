/** In-memory pin choices for the guide's tool cards. */
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store';
/** Tab kind whose card always shows and never enters the customization list. */
export const FIXED_GUIDE_KIND = 'files';
const NO_CARDS = [];
/**
 * Pin identity of one guide card; the kind survives an extension taking over that kind.
 * @param entry - Guide card from the tab registry.
 * @returns `kind:id`.
 */
export function guideEntryId(entry) {
    return `${entry.kind}:${entry.id}`;
}
/**
 * Create the in-memory guide pin list; the snapshot keeps its reference until the list changes.
 * @returns Unpinned card ids and the pin mutation.
 */
export function createGuidePins() {
    const store = createSnapshotStore({ unpinned: NO_CARDS });
    return {
        unpinned: { getSnapshot: () => store.getSnapshot().unpinned, subscribe: listener => store.subscribe(listener) },
        setPinned: (id, pinned) => {
            const current = store.getSnapshot().unpinned;
            if (id.startsWith(`${FIXED_GUIDE_KIND}:`) || pinned !== current.includes(id))
                return;
            store.set({ unpinned: pinned ? current.filter(entry => entry !== id) : [...current, id] });
        },
    };
}
//# sourceMappingURL=guide-pins.js.map