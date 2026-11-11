import { jsx as _jsx } from "react/jsx-runtime";
import { memo } from 'react';
import { GOAL_COMMAND } from "./goal-command-input.js";
import css from './GoalCommandInputView.module.css';
const GOAL_COMMANDS = [{ start: 0, end: GOAL_COMMAND.length + 1, name: GOAL_COMMAND }];
/**
 * Right-aligned `/goal` input bubble without ordinary message actions. The
 * echoed line decorates its leading `/goal` token as a command chip — the run
 * this Node projects is the fact that that token was a command — and keeps
 * the objective, `/goal` mentions included, as plain text.
 */
export const GoalCommandInputView = memo(function GoalCommandInputView({ node, t, renderFactorySlot, }) {
    const data = node.data;
    return (_jsx("div", { className: css.row, "data-command-input": "", role: "group", "aria-label": t('commandInput.aria'), children: _jsx("div", { className: css.stack, children: _jsx("div", { className: css.bubble, children: renderFactorySlot('conversation.user.text', {
                    text: data.text,
                    facts: { commands: GOAL_COMMANDS },
                    interactive: false,
                }) }) }) }));
});
//# sourceMappingURL=GoalCommandInputView.js.map