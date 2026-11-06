import { jsx as _jsx } from "react/jsx-runtime";
import clsx from 'clsx';
import css from './UserMessageBubble.module.css';
/**
 * Render user-style message content without row placement or actions.
 * @param props - Native div attributes and the caller-owned content.
 * @returns A theme-aware message surface that wraps its content.
 */
export function UserMessageBubble({ className, ...props }) {
    return _jsx("div", { ...props, className: clsx(css.bubble, className) });
}
//# sourceMappingURL=UserMessageBubble.js.map