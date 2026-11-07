import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/** Confirmation shown when this page reconnects to a Host running another release. */
import clsx from 'clsx';
import { Button, Modal } from '@codesmith/client-ui-primitives';
import css from './HostReleasePrompt.module.css';
/**
 * Name both ends of one release transition: their versions when those differ, otherwise their commits.
 * @param from - release the transition starts at.
 * @param to - release the transition ends at.
 * @param unknown - text for a release or field the Host did not publish.
 * @returns the start and end text.
 */
function transitionText(from, to, unknown) {
    const field = from?.version !== to?.version ? 'version' : 'commit';
    return [from?.[field] ?? unknown, to?.[field] ?? unknown];
}
/**
 * One labelled transition line. A completed transition fades its start; a pending one fades its end.
 * @param props - row label, both ends, and completion.
 * @returns the row's cells for the shared transition grid.
 */
function Transition({ label, from, to, done }) {
    return (_jsxs(_Fragment, { children: [_jsx("span", { className: css.label, children: label }), _jsx("span", { className: clsx(css.release, done ? css.faded : css.solid), children: from }), _jsx("span", { "aria-hidden": "true", className: clsx(css.line, done ? css.done : css.pending) }), _jsx("span", { className: clsx(css.release, done ? css.solid : css.faded), children: to })] }));
}
/**
 * Ask whether to reload after the Host changed releases; closing the dialog keeps the page.
 * The service line shows the Host's completed switch from its previous release; the page line shows the
 * switch from the page's boot release that only a reload completes.
 * @param props - the release change, both decisions, and localized copy.
 * @returns the dialog while a change is announced; otherwise nothing.
 */
export function HostReleasePrompt({ useReleaseChange, dismissRelease, reloadPage, t }) {
    const change = useReleaseChange(value => value);
    if (change === undefined)
        return null;
    const unknown = t('hostRelease.unknown');
    const [serviceFrom, serviceTo] = transitionText(change.previous, change.host, unknown);
    const [pageFrom, pageTo] = transitionText(change.page, change.host, unknown);
    return (_jsx(Modal, { open: true, title: t('hostRelease.title'), description: t('hostRelease.description'), closeLabel: t('hostRelease.later'), onClose: dismissRelease, className: clsx(css.dialog), footer: (_jsxs(_Fragment, { children: [_jsx(Button, { variant: "outline", "data-modal-autofocus": true, onClick: dismissRelease, children: t('hostRelease.later') }), _jsx(Button, { variant: "primary", onClick: reloadPage, children: t('hostRelease.reload') })] })), children: _jsxs("div", { className: css.transitions, children: [_jsx(Transition, { label: t('hostRelease.service'), from: serviceFrom, to: serviceTo, done: true }), _jsx(Transition, { label: t('hostRelease.page'), from: pageFrom, to: pageTo, done: false })] }) }));
}
//# sourceMappingURL=HostReleasePrompt.js.map