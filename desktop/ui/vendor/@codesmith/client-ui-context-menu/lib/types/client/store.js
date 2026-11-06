/** Single-popup viewing state and feedback retained outside trigger components. */
import { defineStore } from '@codesmith/client-store';
/**
 * Create the root popup store.
 * @returns the presentation and its mutation actions.
 */
export function createMenuStore() {
    return defineStore({
        init: () => ({ occurrence: 0, closing: false, replacing: false, target: null, point: { x: 0, y: 0 }, items: [], notice: null }),
        actions: {
            open(draft, target, point) {
                draft.occurrence++;
                draft.closing = false;
                draft.replacing = false;
                draft.target = target;
                draft.point = point;
                draft.items = [];
            },
            items(draft, items) { draft.items = items; },
            exit(draft, replacing) { draft.closing = true; draft.replacing = replacing; },
            close(draft) { draft.target = null; draft.items = []; },
            notify(draft, notice) { draft.notice = notice; },
        },
    });
}
//# sourceMappingURL=store.js.map