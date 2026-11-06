import { jsx as _jsx } from "react/jsx-runtime";
/** File-drag failure presentation in the existing application overlay. */
import { Toast } from '@deepseek-ai/dsh-client-ui-primitives';
/**
 * Keep transient failures visible after their originating source unmounts.
 * @param props - framework-bound failure source and dismissal callback.
 * @returns the current failure toast, or nothing.
 */
export function FileDragFeedback({ useFailure, dismiss }) {
    const failure = useFailure(value => value);
    return failure === null ? null : _jsx(Toast, { text: failure.text, onDone: dismiss }, failure.sequence);
}
//# sourceMappingURL=feedback.js.map