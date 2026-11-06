/**
 * Map a preset id to its fixed analytics category; custom ids never leave the Client.
 * @param id - Agent preset id the Session records.
 * @returns the built-in category, or other.
 */
function analyticsPreset(id) {
    switch (id) {
        case 'cordis': return 'creator';
        case 'ptc': return 'ptc';
        case 'minimal': return 'minimal';
        case 'standard': return 'standard';
        default: return 'other';
    }
}
/**
 * Report the original message occurrence without reading newer Session facts.
 * @param ctx - client context.
 * @param submission - original occurrence and Session snapshot.
 */
export function reportMessageSubmission(ctx, submission) {
    const { state, timestamp, mode } = submission;
    if (state === undefined)
        return;
    const model = state.model;
    ctx.get('productAnalytics')?.track('send_button_click', {
        ...state.sessionId === undefined ? {} : { session_id: state.sessionId },
        ...model === undefined ? {} : { model_name: `${model.provider}/${model.name}`, ...model.effort === undefined ? {} : { thinking_effort: model.effort } },
        run_mode: state.runMode,
        msg_type: state.running ? mode : 'default',
        is_new_session: state.newSession,
        ...state.agentPreset === undefined ? {} : { agent_preset: analyticsPreset(state.agentPreset) },
    }, timestamp);
}
//# sourceMappingURL=submission-analytics.js.map