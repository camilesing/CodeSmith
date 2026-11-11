import { jsx as _jsx } from "react/jsx-runtime";
import { createSnapshotStore } from '@codesmith/client-store';
import { IconWarningOutlineRegular, isMobileDevice, Toast } from '@codesmith/client-ui-primitives';
/**
 * Create the one outcome store the overlay entry shows.
 * @returns the observable notice with its report and dismiss actions.
 */
export function createOpenInAppToastSource() {
    const store = createSnapshotStore(null);
    let seq = 0;
    return {
        hooks: { toast: store },
        report: (outcome) => {
            if (outcome.kind === 'skipped')
                return;
            if (outcome.kind !== 'failed' && !isMobileDevice())
                return;
            seq += 1;
            store.set({ ...outcome, seq });
        },
        dismiss: () => { store.set(null); },
    };
}
/**
 * Render a mobile handoff confirmation or a native-open failure.
 * @param props - the notice hook, its dismissal, and the locale seat.
 * @returns the banner on display, or null.
 */
export function OpenInAppToast({ useToast, dismiss, t }) {
    const toast = useToast(current => current);
    if (toast === null)
        return null;
    const key = `open-in-app-${String(toast.seq)}`;
    if (toast.kind === 'failed') {
        return _jsx(Toast, { text: t(toast.failure === 'revealError' ? 'path.revealError' : 'path.openError'), icon: _jsx(IconWarningOutlineRegular, {}), onDone: dismiss }, key);
    }
    return _jsx(Toast, { text: t(toast.kind === 'opened' ? 'path.opened' : 'path.revealed'), tone: "success", onDone: dismiss }, key);
}
//# sourceMappingURL=open-toast.js.map