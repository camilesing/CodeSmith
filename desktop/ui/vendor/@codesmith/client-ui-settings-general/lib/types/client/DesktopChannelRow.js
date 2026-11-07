import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Desktop channel selection; persistence and updater transitions remain in Electron. */
import { useState } from 'react';
import { Menu, Toast, IconWarningOutlineRegular, IconLoadingOutlineRegular, IconChevronDownOutlineRegular } from '@codesmith/client-ui-primitives';
import css from './DesktopChannelRow.module.css';
/**
 * Render the Desktop-only channel control without depending on a Host settings service.
 * @param props - Shell observation, selection action and localized copy.
 * @returns The General settings row.
 */
export function DesktopChannelRow({ useChannel, select, t }) {
    const { state, failed, pending } = useChannel(value => value);
    const [open, setOpen] = useState(false);
    const busy = pending !== undefined || state?.changing === true;
    const channel = pending ?? state?.channel;
    return _jsxs("div", { className: css.row, role: "group", "aria-label": t('desktop.channel.title'), children: [_jsxs("div", { className: css.rowText, children: [_jsx("div", { className: css.title, children: t('desktop.channel.title') }), _jsx("div", { className: css.description, children: t('desktop.channel.description') }), (failed || state?.error !== undefined) && _jsx("div", { role: "alert", children: t(failed ? 'desktop.channel.readError' : 'desktop.channel.error') })] }), _jsx(Menu, { open: open, onClose: () => { setOpen(false); }, portal: true, align: "end", selectedId: channel ?? '', items: [{ id: 'stable', label: t('desktop.channel.stable') }, { id: 'nightly', label: t('desktop.channel.nightly') }], onSelect: (id) => {
                    setOpen(false);
                    void select(id);
                }, anchor: _jsxs("button", { type: "button", className: css.selector, disabled: busy || state === undefined, "aria-busy": busy, "aria-label": t('desktop.channel.title'), "aria-haspopup": "menu", "aria-expanded": open, onClick: () => { setOpen(!open); }, children: [channel === undefined || channel === null
                            ? t('desktop.channel.unavailable') : t(channel === 'stable' ? 'desktop.channel.stable' : 'desktop.channel.nightly'), busy ? _jsx(IconLoadingOutlineRegular, { className: css.spinner }) : _jsx(IconChevronDownOutlineRegular, { className: css.chevron })] }) })] });
}
/**
 * Render the latest channel save result outside Settings.
 * @param props - Shared operation state, dismissal and localized copy.
 * @returns A transient toast, or nothing.
 */
export function DesktopChannelToast({ useChannel, dismiss, openConfiguration, t }) {
    const notice = useChannel(value => value.notice);
    if (notice === undefined)
        return null;
    return _jsx(Toast, { text: t(`desktop.channel.result.${notice.outcome}`), ...notice.outcome === 'configuration' ? { holdMs: 10_000,
            actions: [{ label: t('desktop.channel.details'), onClick: openConfiguration }] } : {}, onDone: () => { dismiss(notice.sequence); }, ...notice.outcome === 'applied' ? { tone: 'success' } : { icon: _jsx(IconWarningOutlineRegular, {}) } }, notice.sequence);
}
//# sourceMappingURL=DesktopChannelRow.js.map