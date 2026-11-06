import { jsx as _jsx } from "react/jsx-runtime";
import { memo, useCallback, useMemo } from 'react';
import { AssistantMarkdown } from "./AssistantMarkdown.js";
/** Streaming, settled, and interrupted Assistant states share one keyed renderer instance. */
export const AssistantNodeView = memo(function AssistantNodeView({ node, groupPart, useDisclosure, useGroupAction, useTurnData, turnProcess, openFile, loadImage, fileMentions, usePresentation, renderSlot, t, sessionId, useContextMenu, resolveFilePath, }) {
    const { getTriggerProps } = useContextMenu();
    const getFileTriggerProps = useCallback((path) => getTriggerProps(() => ({
        kind: 'file', value: { sessionId, path: resolveFilePath(path) },
    })), [getTriggerProps, sessionId, resolveFilePath]);
    const data = node.data;
    const turn = node.location.kind === 'turn' || node.location.kind === 'step'
        ? node.location.turn
        : undefined;
    const tail = useTurnData('turn-tail');
    const owner = useMemo(() => {
        if (turn?.status !== 'closed' || data.finalNode === undefined)
            return undefined;
        if (tail?.closing?.finalNode.seq !== data.finalNode.seq)
            return undefined;
        return { turn, seq: data.finalNode.seq, openFile };
    }, [data.finalNode, openFile, tail, turn]);
    const mentions = useMemo(() => owner === undefined ? undefined : fileMentions(owner, getFileTriggerProps), [fileMentions, owner, getFileTriggerProps]);
    const keepReplies = usePresentation(policy => policy.foldedReplies === 'all');
    // A kept reply row stays visible while the folded Turn hides its inline reasoning.
    const reasoningHidden = turnProcess !== undefined
        && turnProcess.foldable
        && (keepReplies
            ? groupPart !== 'reasoning'
            : turnProcess.spec.answerStep === data.step && turnProcess.spec.inlineReasoning)
        && !turnProcess.open;
    const revealProcess = useCallback(() => { turnProcess?.setOpen(true); }, [turnProcess]);
    return (_jsx(AssistantMarkdown, { blocks: data.blocks, groupPart: groupPart, useDisclosure: useDisclosure, streaming: data.status === 'running', textInteractionSource: data.status === 'settled' && data.finalNode?.messageId !== undefined, interrupted: data.status === 'interrupted', loadImage: loadImage, reasoningHidden: reasoningHidden, useGroupAction: useGroupAction, usePresentation: usePresentation, renderSlot: renderSlot, revealProcess: revealProcess, mentions: mentions, t: t }));
});
//# sourceMappingURL=AssistantNodeView.js.map