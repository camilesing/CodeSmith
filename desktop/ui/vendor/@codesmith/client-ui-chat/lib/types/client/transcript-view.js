/** Independent work-details preferences backed by the same Host configuration form. */
import { createSnapshotStore } from '@codesmith/client-store';
import { COLLAPSE_TIMING_FIELD, DEFAULT_COLLAPSE_TIMING, DEFAULT_FOLDED_REPLIES, FOLDED_REPLIES_FIELD, DEFAULT_TRANSCRIPT_VIEW_MODE, LEGACY_TRANSCRIPT_VIEW_MODE, LEGACY_EXPANDED_TRANSCRIPT_VIEW_MODE, TRANSCRIPT_VIEW_FIELD, } from "../chat-settings.js";
/** Live work-details preference consumed by Chat and its Settings row. */
export class TranscriptViewPolicy {
    host;
    defaultMode;
    unsubscribe;
    /** Reactive current mode, including the client default before Host settings arrive. */
    mode;
    /** Reactive collapse timing, defaulting to completion until Host settings arrive. */
    collapseTiming = createSnapshotStore(DEFAULT_COLLAPSE_TIMING);
    /** Reactive folded-reply choice, keeping every reply until Host settings arrive. */
    foldedReplies = createSnapshotStore(DEFAULT_FOLDED_REPLIES);
    /**
     * @param host - durable Chat settings scope.
     * @param defaultMode - presentation used without an explicit saved mode.
     */
    constructor(host, defaultMode = DEFAULT_TRANSCRIPT_VIEW_MODE) {
        this.host = host;
        this.defaultMode = defaultMode;
        this.mode = createSnapshotStore(defaultMode);
        this.unsubscribe = host.subscribe(() => { this.adopt(); });
        this.adopt();
    }
    /** Release the accepted-value subscription. */
    dispose() { this.unsubscribe(); }
    /**
     * Publish and persist one explicit user choice.
     * @param mode - Compact, Standard, Detailed, or Verbose work details.
     */
    setMode(mode) {
        if (this.mode.getSnapshot() === mode)
            return;
        this.mode.set(mode);
        void this.host.set(TRANSCRIPT_VIEW_FIELD, mode);
    }
    /**
     * Publish and persist collapse timing without changing the work-details mode.
     * @param timing - immediate completion folding or deferred folding at the next input.
     */
    setCollapseTiming(timing) {
        if (this.collapseTiming.getSnapshot() === timing)
            return;
        this.collapseTiming.set(timing);
        void this.host.set(COLLAPSE_TIMING_FIELD, timing);
    }
    /**
     * Publish and persist which Assistant replies stay visible in a folded Turn.
     * @param replies - every reply, or only the final answer.
     */
    setFoldedReplies(replies) {
        if (this.foldedReplies.getSnapshot() === replies)
            return;
        this.foldedReplies.set(replies);
        void this.host.set(FOLDED_REPLIES_FIELD, replies);
    }
    /** Adopt the latest accepted Host section without writing it back. */
    adopt() {
        const section = this.host.getSnapshot().value;
        if (section === undefined)
            return;
        const saved = section.transcriptView;
        const mode = saved === LEGACY_TRANSCRIPT_VIEW_MODE || saved === LEGACY_EXPANDED_TRANSCRIPT_VIEW_MODE
            ? 'detailed' : saved ?? this.defaultMode;
        if (this.mode.getSnapshot() !== mode)
            this.mode.set(mode);
        const timing = section.collapseTiming ?? DEFAULT_COLLAPSE_TIMING;
        if (this.collapseTiming.getSnapshot() !== timing)
            this.collapseTiming.set(timing);
        const replies = section.foldedReplies ?? DEFAULT_FOLDED_REPLIES;
        if (this.foldedReplies.getSnapshot() !== replies)
            this.foldedReplies.set(replies);
    }
}
//# sourceMappingURL=transcript-view.js.map