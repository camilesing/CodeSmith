import { ConversationDefinitionRegistry } from "./definition-registry.js";
/** Effect-owned registrations; active surfaces belong to individual Chat views. */
export class ConversationInteractionRegistry extends ConversationDefinitionRegistry {
    ids = [];
    /**
     * Register a synchronous handler and its keyed renderer identity.
     * @param definition - uniquely named plugin handler.
     * @returns idempotent disposer tied to the registering plugin's lifetime.
     */
    register(definition) {
        if (definition.id === '')
            throw new Error('text interaction ID must not be empty');
        return this.registerDefinition(definition.id, definition, `Duplicate text interaction: ${definition.id}`, `uiConversation.interactions.register(${JSON.stringify(definition.id)})`);
    }
    /**
     * Offer a gesture until one handler opens its surface; opening never prevents browser defaults.
     * @param sessionId - conversation where the gesture occurred.
     * @param gesture - synchronous access to the browser event and DOM text.
     * @returns selected renderer ID, or null when no plugin opens a surface.
     */
    dispatch(sessionId, gesture) {
        for (const definition of this.entries()) {
            const state = { opened: false };
            let dispatching = true;
            try {
                definition.onInteraction({
                    ...gesture, sessionId,
                    open: () => { if (dispatching)
                        state.opened = true; },
                    preventDefault: () => { if (dispatching)
                        gesture.preventDefault(); },
                });
            }
            finally {
                dispatching = false;
            }
            if (state.opened)
                return definition.id;
        }
        return null;
    }
    /**
     * Observe registration changes so Chat views can dismiss stale surfaces.
     * @returns stable IDs until the registry changes.
     */
    getSnapshot() {
        return this.ids;
    }
    refresh() {
        this.ids = [...this.definitions.keys()];
        super.refresh();
    }
}
//# sourceMappingURL=interaction-registry.js.map