import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * The guide tab's body: a chain host, and the guide it falls back to.
 *
 * The chain is the replacement seam. A product with its own idea of what an
 * empty sidebar should say registers into `sidebar.right.tab.guide`, and its entry
 * takes the whole body; with no entry, or with every entry declining, the guide
 * below renders. The shipped guide is the owner's fallback rather than a chain
 * entry of its own, so there is always exactly one body and the shipped one
 * cannot be outvoted by accident.
 *
 * The shipped guide is a muted compass over the pinned entry capsules every
 * registered type contributed, centred in the body, with no heading, as a
 * browser start page shows its doors without a caption. While at most four
 * capsules are visible, a capsule with a description shows it under the title;
 * a longer list drops every description to stay light. A More card follows
 * while `sidebar.right.tab.guide.actions` has registrations.
 * Picking one opens that type as a page
 * in this tab's place, so the guide is a doorway rather than a page that stays
 * open.
 */
import { useState } from 'react';
import { Menu, ShortcutKeys } from '@deepseek-ai/dsh-client-ui-primitives';
import { CompassGlyph, CubeGlyph } from "./GuideTitle.js";
import { GuideMoreIcon } from "./GuideMoreIcon.js";
import { FIXED_GUIDE_KIND, guideEntryId } from "./guide-pins.js";
import css from './GuideBody.module.css';
/** Entry count past which the guide drops the capsules' descriptions to stay light. */
const MAX_DESCRIBED_ENTRIES = 4;
/** One entry capsule: the contributing type's glyph and title, and its description while the guide is short. */
function EntryBox({ entry, described, onPick, shortcut }) {
    const Icon = entry.icon ?? CubeGlyph;
    const description = described ? entry.description?.() : undefined;
    return (_jsxs("button", { type: "button", className: css.entry, "data-sidebar-right-guide-entry": entry.kind, "aria-keyshortcuts": shortcut?.aria, onClick: () => { onPick(entry); }, children: [_jsx("span", { className: css.entryIcon, children: _jsx(Icon, { size: description === undefined ? 22 : 26, className: entry.icon === undefined ? css.placeholderInk : undefined }) }), _jsxs("span", { className: css.entryText, children: [_jsx("span", { className: css.entryTitle, children: entry.title() }), description !== undefined && _jsx("span", { className: css.entryDescription, children: description })] }), shortcut !== undefined && shortcut.keys.length > 0 && _jsx(ShortcutKeys, { keys: shortcut.keys })] }));
}
/** The guide's More card; its menu rows come from `sidebar.right.tab.guide.actions` and span at least the card width. */
function GuideMoreMenu({ label, entries, onOpen, onPinnedChange, renderSlot }) {
    const [open, setOpen] = useState(false);
    const onDismiss = () => { setOpen(false); };
    return (_jsx(Menu, { open: open, onClose: onDismiss, portal: true, autoFocus: true, minWidthFromAnchor: true, align: "end", className: css.moreAnchor, listClassName: css.moreMenu, anchor: (_jsxs("button", { type: "button", className: css.entry, "aria-label": label, "aria-haspopup": "menu", "aria-expanded": open, onClick: () => { setOpen(value => !value); }, children: [_jsx("span", { className: css.entryIcon, children: _jsx(GuideMoreIcon, {}) }), _jsx("span", { className: css.entryText, children: _jsx("span", { className: css.entryTitle, children: label }) })] })), children: renderSlot('sidebar.right.tab.guide.actions', { entries, onOpen, onPinnedChange, onDismiss }) }));
}
/** The shipped guide: the tab's own compass over the doors out of the column. */
function ShippedGuide({ children }) {
    return (_jsxs("div", { className: css.guide, "data-sidebar-right-guide": true, children: [_jsx("span", { className: css.hero, "aria-hidden": "true", children: _jsx(CompassGlyph, { size: 56 }) }), children] }));
}
/** The guide tab's body, replaceable through its chain child. */
export function GuideBody({ useTabInfo, useGuideEntries, renderSlot, renderSlotChain, useShortcuts, useEntryPins, useEntryActions, setPinned, t, }) {
    const shortcuts = useShortcuts(entries => entries);
    const { tab } = useTabInfo();
    const entries = useGuideEntries(entries => entries);
    const unpinned = useEntryPins(ids => ids);
    const hasEntryActions = useEntryActions(registered => registered);
    // A new page replaces the guide; a page already in its pane is revealed, and the guide then closes itself.
    const open = (kind) => {
        tab.actions.openTab(kind, { replaceTab: true });
        tab.actions.close();
    };
    const visibleEntries = entries.filter(entry => entry.kind === FIXED_GUIDE_KIND || !unpinned.includes(guideEntryId(entry)));
    const options = {
        hookContext: useTabInfo,
        fallback: (_jsxs(ShippedGuide, { children: [visibleEntries.map((entry) => {
                    const described = visibleEntries.length <= MAX_DESCRIBED_ENTRIES;
                    const description = described ? entry.description?.() : undefined;
                    return _jsx("div", { className: css.entryCell, children: renderSlot('sidebar.right.tab.guide.entry', {
                            entryId: entry.id, kind: entry.kind, title: entry.title(),
                            ...description === undefined ? {} : { description },
                        }, {
                            entryKey: entry.providerId, hookContext: useTabInfo,
                            fallback: _jsx(EntryBox, { entry: entry, described: described, shortcut: shortcuts.find(shortcut => shortcut.id === entry.commandId), onPick: (selected) => { open(selected.kind); } }),
                        }) }, JSON.stringify([entry.providerId, entry.id]));
                }), hasEntryActions && (_jsx(GuideMoreMenu, { label: t('tab.guide.more'), entries: entries.filter(entry => entry.kind !== FIXED_GUIDE_KIND)
                        .map(entry => ({ id: guideEntryId(entry), label: entry.title(), pinned: !unpinned.includes(guideEntryId(entry)) })), onOpen: (id) => {
                        const entry = entries.find(candidate => guideEntryId(candidate) === id);
                        if (entry !== undefined)
                            open(entry.kind);
                    }, onPinnedChange: setPinned, renderSlot: renderSlot }))] })),
    };
    return renderSlotChain('sidebar.right.tab.guide', {}, options);
}
//# sourceMappingURL=GuideBody.js.map