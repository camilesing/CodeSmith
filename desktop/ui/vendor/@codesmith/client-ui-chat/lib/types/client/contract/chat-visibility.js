/**
 * Exclude system prompts, ordinary Context, permission commands, and normal Team toggle receipts.
 * Failed Team toggles and Context containing tool changes remain visible.
 * @param node - projected Chat node.
 * @returns whether the node contributes a visible Chat row.
 */
export function isVisibleChatNode(node) {
    if (node.kind === 'command') {
        if (node.data.name === 'permission')
            return false;
        if (node.data.name === 'team' && node.data.outcome?.kind !== 'error') {
            const selection = node.data.args?.trim();
            if (selection === 'on' || selection === 'off')
                return false;
        }
    }
    return node.visibility === 'visible'
        && node.kind !== 'system-prompt'
        && (node.kind !== 'context'
            || node.data.content.some(block => block.type === 'tool-addition' || block.type === 'tool-removal'));
}
//# sourceMappingURL=chat-visibility.js.map