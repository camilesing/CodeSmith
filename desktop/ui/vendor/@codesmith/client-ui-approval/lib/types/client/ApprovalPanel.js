/**
 * Render the shared card using the tool approval's one-shot decision policy.
 * @param props Selector-matched request and standard Slot props.
 * @returns The approval composer takeover.
 */
export function ApprovalPanel(props) {
    const approval = props.matched;
    const reason = approval.displayReason === undefined ? approval.reason : props.resolveReason(approval.displayReason);
    return props.renderFactorySlot('conversation.approval.card', {
        requestKey: approval.key,
        title: reason ?? props.t('escalation', { toolName: approval.toolName }),
        ...(approval.callId === undefined ? {} : { callId: approval.callId }),
        canAnswer: () => approval.answerable,
        onAllow: () => approval.answer('allowed-once'),
        onReject: () => approval.answer('rejected'),
    });
}
//# sourceMappingURL=ApprovalPanel.js.map