/** Legacy external Session contributions shared by the row, header, and More trigger. */
import { useMemo } from 'react';
/** @param props - captured Session and the existing extension position. @returns legacy component contributions. */
export function SessionMenu({ target, dismiss, renderSlot }) {
    const hookContext = useMemo(() => [true, (open) => { if (!open)
            dismiss(); }], [dismiss]);
    if (target.kind !== 'session')
        return null;
    return renderSlot('sidebar.workspaces.session.menu.item', target.value, {
        hookContext,
    });
}
//# sourceMappingURL=SessionMenu.js.map