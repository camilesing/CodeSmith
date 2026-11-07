import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Inline exit status for a settled command. */
import { Tag } from '@codesmith/client-ui-primitives';
import css from './ExitCodeBadge.module.css';
/**
 * Render a neutral capsule with a failure dot and a localized exit status.
 * @param props - localized exit code or terminating signal label.
 * @returns the exit status capsule.
 */
export function ExitCodeBadge({ label }) {
    return (_jsxs(Tag, { tone: "neutral", className: css.badge, children: [_jsx("span", { className: css.dot, "aria-hidden": true }), label] }));
}
//# sourceMappingURL=ExitCodeBadge.js.map