/** Shared native-open status for delivery cards, the changed-files card, and closing-message file mentions. */
import { createSnapshotStore } from '@codesmith/client-store';
import { changedFileUrl } from "../changes.js";
import { presentedFileUrl, PRESENT_HOST_ROUTE, isPresentedHost } from "../presented.js";
/** One browser plugin's file-open requests, cancelled when that plugin is disposed. */
export class PresentedOpenController {
    /** File action URLs key the state across Sessions, turns, and both clickable surfaces. */
    state = createSnapshotStore({});
    /** Native destination metadata, or a retryable read failure. */
    host = createSnapshotStore(null);
    loading;
    metadata = new AbortController();
    lifetime = new AbortController();
    pending = new Set();
    /**
     * Open a declared file once while a request for the same coordinates is pending.
     * Failures remain visible on the card and a later gesture retries them.
     * @param sessionId - viewed Session, including a fork's own identity.
     * @param seq - durable delivery event sequence.
     * @param index - original file index within that event.
     * @param action - default application open or file-manager reveal.
     * @param application - registered handler identifier for an explicit application choice.
     * @returns the settled outcome after publishing card status; a dropped
     * request reports `{ kind: 'skipped' }` and announces nothing.
     */
    open(sessionId, seq, index, action = 'open', application) {
        return this.openUrl(presentedFileUrl(sessionId, seq, index), action, application);
    }
    /**
     * Open one recorded changed file in the Host's default application.
     * @param sessionId - viewed Session.
     * @param seq - durable workspace/changes event sequence.
     * @param index - original file index within that event.
     * @param action - application open or file-manager reveal.
     * @param application - registered handler identifier for an explicit application choice.
     * @returns the settled outcome after publishing card status; a dropped
     * request reports `{ kind: 'skipped' }` and announces nothing.
     */
    openChanged(sessionId, seq, index, action = 'open', application) {
        return this.openUrl(changedFileUrl(sessionId, seq, index), action, application);
    }
    async openUrl(url, action, application) {
        const phase = this.state.getSnapshot()[url];
        if (this.lifetime.signal.aborted || phase === 'opening' || phase === 'revealing')
            return { kind: 'skipped' };
        this.state.update((state) => { state[url] = action === 'open' ? 'opening' : 'revealing'; });
        const task = this.request(url, action, application);
        this.pending.add(task);
        try {
            return await task;
        }
        finally {
            this.pending.delete(task);
        }
    }
    /**
     * Read the serving desktop metadata, coalescing concurrent reads; a later call retries failure.
     * @returns after metadata or a retryable error is published.
     */
    async loadHost() {
        if (this.lifetime.signal.aborted)
            return;
        if (this.loading !== undefined)
            return this.loading;
        this.host.set(null);
        const task = this.readHost(AbortSignal.any([this.lifetime.signal, this.metadata.signal]));
        this.loading = task;
        this.pending.add(task);
        try {
            await task;
        }
        finally {
            if (this.loading === task)
                this.loading = undefined;
            this.pending.delete(task);
        }
    }
    /** Invalidate desktop metadata on connection replacement; mounted cards request the new Host. */
    resetHost() {
        const wasLoading = this.loading !== undefined;
        this.metadata.abort();
        this.metadata = new AbortController();
        this.loading = undefined;
        this.host.set(null);
        if (wasLoading)
            void this.loadHost();
    }
    async readHost(signal) {
        let host = 'error';
        try {
            const response = await fetch(PRESENT_HOST_ROUTE, { signal });
            if (response.ok) {
                const value = await response.json();
                if (isPresentedHost(value))
                    host = value;
            }
        }
        catch {
            host = 'error';
        }
        if (!signal.aborted)
            this.host.set(host);
    }
    /** Cancel outstanding requests, suppress their announcements, and wait until they settle. */
    async dispose() {
        this.lifetime.abort();
        await Promise.all(this.pending);
    }
    async request(url, action, application) {
        const failure = action === 'open' ? 'error' : 'revealError';
        let phase;
        try {
            const target = action === 'reveal' ? `${url}&action=reveal`
                : application === undefined ? url : `${url}&application=${encodeURIComponent(application)}`;
            const response = await fetch(target, { method: 'POST', signal: this.lifetime.signal });
            if (!response.ok)
                phase = response.status === 422 ? 'nativeUnavailable' : failure;
        }
        catch {
            // Transport failures share the retryable card state with Host open failures.
            phase = failure;
        }
        if (!this.lifetime.signal.aborted) {
            this.state.update((state) => {
                if (phase === undefined)
                    Reflect.deleteProperty(state, url);
                else
                    state[url] = phase;
            });
        }
        if (this.lifetime.signal.aborted)
            return { kind: 'skipped' };
        if (phase === undefined)
            return { kind: action === 'open' ? 'opened' : 'revealed' };
        return { kind: 'failed', failure: action === 'reveal' ? 'revealError' : 'openError' };
    }
}
//# sourceMappingURL=present-open.js.map