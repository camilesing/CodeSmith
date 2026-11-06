/** In-memory pin choices for the Conversation View tabs. */
import { createSnapshotStore } from '@codesmith/client-store';
/** View whose tab always shows and never enters the customization list. */
export const FIXED_VIEW_ID = 'chat';
const NO_VIEWS = [];
/**
 * Create the in-memory View pin list; the snapshot keeps its reference until the list changes.
 * @returns Unpinned View ids and the pin mutation.
 */
export function createViewPins() {
    const store = createSnapshotStore({ unpinned: NO_VIEWS });
    return {
        unpinned: { getSnapshot: () => store.getSnapshot().unpinned, subscribe: listener => store.subscribe(listener) },
        setPinned: (id, pinned) => {
            const current = store.getSnapshot().unpinned;
            if (id === FIXED_VIEW_ID || pinned !== current.includes(id))
                return;
            store.set({ unpinned: pinned ? current.filter(entry => entry !== id) : [...current, id] });
        },
    };
}
//# sourceMappingURL=view-pins.js.map