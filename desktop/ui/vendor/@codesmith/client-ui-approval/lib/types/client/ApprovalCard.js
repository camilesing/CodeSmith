import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Shared approval presentation; each caller retains its decision policy and answer channel. */
import { useEffect, useRef, useState } from 'react';
import { Button, StateDot } from '@codesmith/client-ui-primitives';
import css from './ApprovalPanel.module.css';
/**
 * Render one decision with caller-owned actions and an optional persistent choice.
 * @param props Request identity, copy, actions and correlated detail.
 * @returns The shared approval card, reset for each request.
 */
export function ApprovalCard(props) {
    return _jsx(ApprovalFlow, { ...props }, props.requestKey);
}
function ApprovalFlow(props) {
    const { requestKey, title, description, subject, allowLabel, canAnswer, onAllow, onReject, onAlways, t } = props;
    const detail = props.callId === undefined ? null : props.renderSlot('conversation.approval.detail', { callId: props.callId });
    const [answered, setAnswered] = useState(false);
    const waiting = useRef(false);
    const active = useRef(true);
    const composing = useRef(false);
    const compositionEnded = useRef(false);
    useEffect(() => {
        active.current = true;
        return () => { active.current = false; };
    }, []);
    const answer = (send) => {
        if (waiting.current || !canAnswer())
            return;
        waiting.current = true;
        setAnswered(true);
        void send().catch(() => {
            if (!active.current || !canAnswer())
                return;
            waiting.current = false;
            setAnswered(false);
        });
    };
    const keydown = (event) => {
        const element = event.target;
        if (event.defaultPrevented || !event.currentTarget.contains(document.activeElement)
            || element.closest('input, textarea, select, [contenteditable="true"], [contenteditable=""]') !== null)
            return;
        if (event.key !== 'Enter' && event.key !== 'Escape')
            return;
        if (event.key === 'Enter' && element.closest('button, a[href], [role="button"]') !== null)
            return;
        if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey)
            return;
        event.preventDefault();
        event.stopPropagation();
        // oxlint-disable-next-line typescript/no-deprecated -- IME 229 covers engines without isComposing.
        if (event.repeat || composing.current || compositionEnded.current || event.nativeEvent.isComposing || event.keyCode === 229)
            return;
        answer(event.key === 'Enter' ? onAllow : onReject);
    };
    return (_jsx("div", { className: css.root, "data-approval-key": requestKey, "aria-busy": answered, onKeyDown: keydown, onKeyUpCapture: () => { compositionEnded.current = false; }, onCompositionStartCapture: () => { composing.current = true; }, onCompositionEndCapture: () => { composing.current = false; compositionEnded.current = true; }, children: _jsxs("div", { className: subject === undefined ? css.card : `${css.card} ${css.withSubject}`, children: [_jsxs("div", { className: css.strip, children: [_jsx(StateDot, { state: answered ? 'ongoing' : 'warning' }), t('waiting')] }), _jsxs("div", { className: css.body, "data-approval-scroll": "", tabIndex: 0, role: "group", "aria-label": t('detail.aria'), children: [subject !== undefined && _jsxs("div", { className: css.subject, children: [subject.icon !== undefined && _jsx("img", { src: subject.icon, alt: "", width: 40, height: 40, draggable: false }), _jsx("strong", { children: subject.name })] }), _jsx("div", { className: css.headline, children: title }), description !== undefined && _jsx("p", { className: css.description, children: description }), detail !== null && _jsx("div", { className: css.command, children: detail })] }), _jsxs("div", { className: css.actionRow, children: [_jsx(Button, { variant: "outline", className: css.reject, disabled: answered, onClick: () => { answer(onReject); }, children: t('reject') }), _jsx(Button, { variant: onAlways === undefined ? 'primary' : 'outline', disabled: answered, onClick: () => { answer(onAllow); }, children: allowLabel ?? t('allowOnce') }), onAlways !== undefined && _jsx(Button, { variant: "primary", disabled: answered, onClick: () => { answer(onAlways); }, children: t('allowAlways') })] })] }) }));
}
//# sourceMappingURL=ApprovalCard.js.map