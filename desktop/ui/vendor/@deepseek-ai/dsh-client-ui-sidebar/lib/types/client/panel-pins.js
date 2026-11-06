/** In-memory pin choices for the global panel launchers. */
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store';
/** Panel whose launcher always shows and never enters the customization list. */
export const FIXED_PANEL_ID = 'plugins';
const NO_PANELS = [];
/**
 * Create the in-memory panel pin list; the snapshot keeps its reference until the list changes.
 * @returns Unpinned panel ids and the pin mutation.
 */
export function createPanelPins() {
    const store = createSnapshotStore({ unpinned: NO_PANELS });
    return {
        unpinned: { getSnapshot: () => store.getSnapshot().unpinned, subscribe: listener => store.subscribe(listener) },
        setPinned: (id, pinned) => {
            const current = store.getSnapshot().unpinned;
            if (id === FIXED_PANEL_ID || pinned !== current.includes(id))
                return;
            store.set({ unpinned: pinned ? current.filter(entry => entry !== id) : [...current, id] });
        },
    };
}
//# sourceMappingURL=panel-pins.js.map