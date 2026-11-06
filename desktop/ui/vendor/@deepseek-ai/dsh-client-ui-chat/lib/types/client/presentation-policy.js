/** Chat presentation derived from Host-backed detail, collapse timing, and folded replies. */
import { DEFAULT_FOLDED_REPLIES } from "../chat-settings.js";
export { COLLAPSE_TIMINGS, FOLDED_REPLIES } from "../chat-settings.js";
const POLICIES = {
    compact: {
        mode: 'compact',
        foldCompletedTurns: true,
        stepGrouping: 'collapsed',
        liveProcessDetail: false,
        settledReasoningPreview: false,
    },
    standard: {
        mode: 'standard',
        foldCompletedTurns: true,
        stepGrouping: 'collapsed',
        liveProcessDetail: true,
        settledReasoningPreview: true,
    },
    detailed: {
        mode: 'detailed',
        foldCompletedTurns: true,
        stepGrouping: 'history',
        liveProcessDetail: true,
        settledReasoningPreview: true,
    },
    verbose: {
        mode: 'verbose',
        foldCompletedTurns: false,
        stepGrouping: 'none',
        liveProcessDetail: false,
        settledReasoningPreview: true,
    },
};
function withTiming(collapseTiming, foldedReplies) {
    return {
        compact: { ...POLICIES.compact, collapseTiming, foldedReplies },
        standard: { ...POLICIES.standard, collapseTiming, foldedReplies },
        detailed: { ...POLICIES.detailed, collapseTiming, foldedReplies },
        verbose: { ...POLICIES.verbose, collapseTiming, foldedReplies },
    };
}
const TIMING_POLICIES = {
    completion: { all: withTiming('completion', 'all'), final: withTiming('completion', 'final') },
    'next-input': { all: withTiming('next-input', 'all'), final: withTiming('next-input', 'final') },
};
/**
 * Resolve a stable policy for one detail mode, collapse timing, and folded-reply choice.
 * @param mode - persisted work-details mode.
 * @param collapseTiming - persisted timing; defaults to immediate completion folding.
 * @param foldedReplies - persisted folded-reply choice; defaults to keeping every reply visible.
 * @returns the same policy object for the same choices.
 */
export function presentationPolicyFor(mode, collapseTiming = 'completion', foldedReplies = DEFAULT_FOLDED_REPLIES) {
    return TIMING_POLICIES[collapseTiming][foldedReplies][mode];
}
/**
 * Combine Host-backed detail, persisted timing, and folded replies without owning subscriptions.
 * @param mode - live work-details mode.
 * @param collapseTiming - live Host-backed collapse timing.
 * @param foldedReplies - live Host-backed folded-reply choice.
 * @returns observable policy whose subscriptions follow all three preferences.
 */
export function derivePresentationPolicy(mode, collapseTiming, foldedReplies) {
    return {
        getSnapshot: () => presentationPolicyFor(mode.getSnapshot(), collapseTiming.getSnapshot(), foldedReplies.getSnapshot()),
        subscribe: (listener) => {
            const unsubscribeMode = mode.subscribe(listener);
            const unsubscribeTiming = collapseTiming.subscribe(listener);
            const unsubscribeReplies = foldedReplies.subscribe(listener);
            return () => { unsubscribeMode(); unsubscribeTiming(); unsubscribeReplies(); };
        },
    };
}
//# sourceMappingURL=presentation-policy.js.map