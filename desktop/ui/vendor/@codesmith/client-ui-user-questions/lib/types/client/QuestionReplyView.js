import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Settled question batches rendered with the shared user-message surface. */
import { memo } from 'react';
import { IconCheckOutlineRegular, IconCopyOutlineRegular, Tooltip, UserMessageBubble, useClipboardCopy, } from '@codesmith/client-ui-primitives';
import { replyAnswerValues, replyClipboardText } from "./question-reply.js";
import css from './QuestionReplyView.module.css';
/** Render recorded questions and answers without an expandable process row. */
export const QuestionReplyView = memo(function QuestionReplyView({ node, t }) {
    return _jsx(QuestionReplyBubble, { data: node.data, t: t });
});
/**
 * Render one answered batch with questions above their corresponding answers.
 * @param props - Reply data and the question locale translator.
 * @returns The right-aligned, fully visible question-and-answer bubble.
 */
export function QuestionReplyBubble({ data, t }) {
    const copyText = replyClipboardText(data, t);
    const { copied, onCopy } = useClipboardCopy(copyText);
    return (_jsxs("div", { className: css.row, "data-question-reply": data.callId, "data-reply-outcome": data.outcome, role: "group", "aria-label": t('reply.label'), children: [_jsx("div", { className: css.bubbleWidth, children: _jsx(UserMessageBubble, { className: css.bubble, children: data.questions.length === 0
                        ? _jsx("p", { className: css.text, children: data.text })
                        : _jsx("dl", { className: css.details, children: data.questions.map((question) => {
                                const values = replyAnswerValues(data, question.id);
                                return _jsxs("div", { children: [_jsxs("dt", { className: css.question, children: [question.header && question.header !== question.question && _jsx("span", { className: css.header, children: question.header }), _jsx("span", { children: question.question })] }), _jsx("dd", { className: css.answer, children: values.length === 0 ? t('reply.skipped') : values.join(', ') })] }, question.id);
                            }) }) }) }), _jsxs("div", { className: css.actions, children: [_jsx("span", { className: css.time, children: new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit', hour12: false }).format(data.time) }), _jsx(Tooltip, { label: copied ? t('copied') : t('copy'), side: "bottom", children: _jsx("button", { type: "button", className: css.action, "aria-label": copied ? t('copied') : t('copy'), onClick: onCopy, children: copied ? _jsx(IconCheckOutlineRegular, {}) : _jsx(IconCopyOutlineRegular, {}) }) })] })] }));
}
//# sourceMappingURL=QuestionReplyView.js.map