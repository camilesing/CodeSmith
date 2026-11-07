/** One shell-owned channel subscription shared by Settings views and their toast. */
import { createSnapshotStore } from '@codesmith/client-store';
/** Client observation; no Host service or configuration file is read here. */
export class DesktopChannelSource {
    bridge;
    /** Stable observation shared by mounted General views and the shell toast. */
    store = createSnapshotStore({ failed: false });
    live = true;
    revision = 0;
    sequence = 0;
    unsubscribe;
    /** @param bridge - Isolated shell API, present only in Desktop. */
    constructor(bridge) {
        this.bridge = bridge;
        this.unsubscribe = bridge.subscribe((state) => {
            if (!this.live)
                return;
            this.revision++;
            this.receive(state);
        });
        void bridge.get().then((state) => {
            if (this.live && this.revision === 0)
                this.receive(state);
        }, () => {
            if (this.live && this.revision === 0)
                this.store.set({ ...this.store.getSnapshot(), failed: true });
        });
    }
    receive(state) {
        const previous = this.store.getSnapshot();
        const ownSaveFailure = previous.pending !== undefined || previous.notice?.outcome === 'uncertain';
        const notify = !ownSaveFailure && state.error !== undefined && state.error !== 'cleanup' && state.error !== previous.state?.error;
        this.store.set({ ...previous, state, failed: false,
            ...(notify ? { notice: { sequence: ++this.sequence, outcome: 'configuration' } } : {}),
        });
    }
    /**
     * Show a pending choice until the shell reports its actual selection.
     * @param channel - Selected feed.
     * @returns After the shell reports the result.
     */
    async select(channel) {
        if (!this.isLive() || this.store.getSnapshot().pending !== undefined)
            return;
        const state = this.store.getSnapshot().state;
        if (state?.channel === channel && state.error === undefined && !state.changing)
            return;
        const revision = this.revision;
        const { notice: _notice, ...previous } = this.store.getSnapshot();
        this.store.set({ ...previous, pending: channel });
        let result;
        try {
            result = await this.bridge.set(channel);
        }
        catch (_error) {
            // An IPC failure has no reliable user-facing detail or confirmed new selection.
        }
        if (!this.isLive())
            return;
        const accept = result !== undefined && revision === this.revision;
        if (accept)
            this.revision++;
        const { pending: _pending, ...current } = this.store.getSnapshot();
        this.store.set({
            ...current,
            ...(accept && result !== undefined ? { state: result.state, failed: false } : {}),
            ...(result?.kind === 'unchanged' ? {} : {
                notice: { sequence: ++this.sequence, outcome: result?.kind === 'applied' ? 'applied' : result?.kind === 'failed' ? result.reason : 'save' },
            }),
        });
    }
    /**
     * Retire only the toast whose animation completed.
     * @param sequence - Notice identity captured by the rendered toast.
     */
    dismissNotice(sequence) {
        const current = this.store.getSnapshot();
        if (current.notice?.sequence !== sequence)
            return;
        const { notice: _notice, ...next } = current;
        this.store.set(next);
    }
    isLive() { return this.live; }
    /** Detach the shell subscription and ignore late IPC responses. */
    dispose() { this.live = false; this.unsubscribe(); }
}
//# sourceMappingURL=desktop-channel-source.js.map