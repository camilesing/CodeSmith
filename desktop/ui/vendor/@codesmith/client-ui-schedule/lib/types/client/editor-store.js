/** Unsaved task forms survive navigation without persisting business records. */
import { defineStore } from '@codesmith/client-store';
const editorActions = {
    setDraft: (state, key, draft) => { state.drafts[key] = draft; },
    clearDraft: (state, key) => {
        state.drafts = Object.fromEntries(Object.entries(state.drafts).filter(([id]) => id !== key));
    },
};
/**
 * Declare drafts retained by the framework for one entry's scope.
 * @returns the entry's store handle.
 */
export function createEditorStore() {
    return defineStore({
        init: () => ({ drafts: {} }),
        actions: editorActions,
    });
}
//# sourceMappingURL=editor-store.js.map