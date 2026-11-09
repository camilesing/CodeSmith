/**
 * Welcome-notice state derived from the welcome settings scope: the notice
 * follows the Host section and records its acknowledgement through it.
 */
import { createSnapshotStore } from '@codesmith/client-store';
import { WELCOME_NOTICE_ACK_FIELD, WELCOME_NOTICE_VERSION, } from "../onboarding-copy.js";
/**
 * Accept any object section verbatim; a malformed durable value reads as an
 * empty section, so the notice treats it as unacknowledged instead of leaving
 * the scope stuck on its previous value.
 * @param section - the wire section value.
 * @returns the section object, or an empty one for non-object values.
 */
export function decodeWelcomeSection(section) {
    return typeof section === 'object' && section !== null && !Array.isArray(section)
        ? section
        : {};
}
/* v8 ignore next 3 -- closed-union default only defends future source widening */
function assertNever(_value) {
    throw new Error('unexpected welcome settings status');
}
/** Coordinates the durable Host acknowledgement. */
export class WelcomeNoticeStore {
    scope;
    /** uSES-safe state source shared by the registered welcome step. */
    store = createSnapshotStore({
        status: 'idle', acknowledged: false, error: null,
    });
    saving = false;
    following;
    constructor(scope) {
        this.scope = scope;
    }
    /**
     * Begin following the bound scope (idempotent) and publish its current answer.
     * @returns settlement after the current answer is published.
     */
    load() {
        this.following ??= this.scope.subscribe(() => { this.derive(); });
        this.derive();
        return Promise.resolve();
    }
    /**
     * Persist this copy version through the scope. Success is judged against the
     * state the write left behind, so a refused or failed write reports false
     * after its recovery read settles.
     * @returns true when the Host holds the acknowledgement.
     */
    async acknowledge() {
        this.saving = true;
        this.store.update((state) => { state.status = 'saving'; state.error = null; });
        try {
            await this.scope.set(WELCOME_NOTICE_ACK_FIELD, WELCOME_NOTICE_VERSION);
        }
        finally {
            this.saving = false;
        }
        this.derive();
        const { acknowledged } = this.store.getSnapshot();
        if (!acknowledged) {
            this.store.update((state) => {
                state.status = 'error';
                state.error = 'the acknowledgement did not persist';
            });
        }
        return acknowledged;
    }
    /** Stop following the scope. */
    dispose() {
        this.following?.();
        this.following = undefined;
    }
    derive() {
        if (this.saving)
            return;
        const scope = this.scope.getSnapshot();
        switch (scope.status) {
            case 'loading':
                this.store.update((state) => { state.status = 'loading'; state.error = null; });
                return;
            case 'unavailable':
                this.store.update((state) => {
                    state.status = 'error';
                    state.acknowledged = false;
                    state.error = 'welcome acknowledgement settings are unavailable';
                });
                return;
            case 'ready': {
                const acknowledged = scope.value?.[WELCOME_NOTICE_ACK_FIELD] === WELCOME_NOTICE_VERSION;
                this.store.update((state) => {
                    state.status = 'ready';
                    state.acknowledged = acknowledged;
                    state.error = null;
                });
                return;
            }
            /* v8 ignore next -- every current settings scope status is handled above */
            default: return assertNever(scope.status);
        }
    }
}
//# sourceMappingURL=welcome-store.js.map