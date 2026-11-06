import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/** Strict per-session header/body content inserted into the resident conversation layout. */
import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import clsx from 'clsx';
import { Button, IconEllipsisOutlineRegular, Menu, Tooltip } from '@codesmith/client-ui-primitives';
import { resolveActiveView } from "../view-selection.js";
import { FIXED_VIEW_ID } from "../view-pins.js";
import { DefaultConversationViews } from "./DefaultConversationViews.js";
import css from './ConversationRoot.module.css';
/** The View tab row's More launcher; its menu rows come from `conversation.session.header.views.actions`. */
function ViewsMoreMenu({ label, current, entries, onOpen, onPinnedChange, renderSlot }) {
    const [open, setOpen] = useState(false);
    const onDismiss = () => { setOpen(false); };
    return (_jsx(Menu, { open: open, onClose: onDismiss, portal: true, autoFocus: true, className: css.viewsMoreAnchor, listClassName: css.viewsMoreMenu, anchor: (_jsx(Tooltip, { label: label, portal: true, children: _jsx(Button, { size: "sm", className: clsx(css.viewsMore, current && css.viewsMoreActive), "aria-label": label, "aria-haspopup": "menu", "aria-expanded": open, onClick: () => { setOpen(value => !value); }, children: _jsx(IconEllipsisOutlineRegular, { size: 13 }) }) })), children: renderSlot('conversation.session.header.views.actions', { entries, onOpen, onPinnedChange, onDismiss }) }));
}
/** Approximate 13px text advance: full-width glyphs take one em, other glyphs about half. */
const WIDE_GLYPH = /[\u1100-\u115F\u2E80-\uA4CF\uAC00-\uD7A3\uF900-\uFAFF\uFE30-\uFE4F\uFF00-\uFF60\uFFE0-\uFFE6]/u;
const WIDE_GLYPH_WIDTH = 13;
const NARROW_GLYPH_WIDTH = 7;
/** Mirrors the `.tabLabel` max-width in `ConversationRoot.module.css`. */
const TAB_LABEL_MAX_WIDTH = 88;
/** Estimate whether a View tab label exceeds its 88px cell. */
function labelNeedsTooltip(label) {
    let width = 0;
    for (const glyph of label)
        width += WIDE_GLYPH.test(glyph) ? WIDE_GLYPH_WIDTH : NARROW_GLYPH_WIDTH;
    return width > TAB_LABEL_MAX_WIDTH;
}
/** One View tab label; its full-name tooltip is offered only when the text is expected to overflow. */
function ViewTabLabel({ label }) {
    return (_jsx(Tooltip, { label: label, portal: true, disabled: !labelNeedsTooltip(label), children: _jsx("span", { className: css.tabLabel, children: label }) }));
}
function deriveAncestry(list, id) {
    const chain = [];
    const seen = new Set();
    let cursor = id;
    while (cursor !== undefined) {
        if (seen.has(cursor))
            break;
        seen.add(cursor);
        const summary = list.byId[cursor];
        if (summary === undefined)
            break;
        chain.unshift({
            id: summary.id,
            displayTitle: summary.displayTitle,
            subagent: summary.origin === 'subagent',
        });
        if (summary.origin !== 'subagent')
            break;
        cursor = summary.parentId;
    }
    return chain;
}
function equalBreadcrumbs(left, right) {
    return left.length === right.length
        && left.every((item, index) => {
            const other = right.at(index);
            return other !== undefined && item.id === other.id && item.displayTitle === other.displayTitle;
        });
}
/**
 * Renders Session header chrome above the resident conversation scrollport.
 * @param props - Strict Session store, view ledger, navigation, render, and locale shares.
 * @returns Session navigation controls, with title and tabs after conversation starts.
 */
export function ConversationSessionHeader({ sessionId, hideChrome, useSessions, useConversationViews, useStore, renderSlot, open, useContextMenu, selectView, setPinned, t, useEntryPins, useEntryActions, }) {
    const { getTriggerProps } = useContextMenu();
    const tabs = useConversationViews(value => value);
    const selectedId = useStore(s => s.view);
    const active = resolveActiveView(tabs, selectedId);
    const ancestry = useSessions(s => deriveAncestry(s, sessionId), equalBreadcrumbs);
    const unpinned = useEntryPins(ids => ids);
    const hasEntryActions = useEntryActions(registered => registered);
    const showTabs = !hideChrome && tabs.length > 1;
    const tabRow = useRef(null);
    const [visibleViewLimit, setVisibleViewLimit] = useState(1);
    // Observe only the available row width; individual tabs are not measured.
    useLayoutEffect(() => {
        const row = tabRow.current;
        if (row === null || !hasEntryActions)
            return;
        const updateLimit = () => {
            setVisibleViewLimit(Math.max(1, Math.floor((row.clientWidth - 48) / 120)));
        };
        updateLimit();
        if (typeof ResizeObserver === 'undefined')
            return;
        const observer = new ResizeObserver(updateLimit);
        observer.observe(row);
        return () => { observer.disconnect(); };
    }, [showTabs, hasEntryActions]);
    const { visibleTabs, moreTabs } = useMemo(() => {
        const preferred = [
            ...tabs.filter(tab => tab.id === FIXED_VIEW_ID),
            ...tabs.filter(tab => tab.id !== FIXED_VIEW_ID && !unpinned.includes(tab.id)),
        ];
        const visibleTabs = hasEntryActions ? preferred.slice(0, visibleViewLimit) : preferred;
        const visibleIds = new Set(visibleTabs.map(tab => tab.id));
        return { visibleTabs, moreTabs: tabs.filter(tab => !visibleIds.has(tab.id)) };
    }, [tabs, unpinned, hasEntryActions, visibleViewLimit]);
    const currentInMore = moreTabs.some(tab => tab.id === active?.id);
    return (_jsxs(_Fragment, { children: [_jsxs("div", { className: css.titleRow, children: [!hideChrome && (_jsxs(_Fragment, { children: [_jsxs("div", { className: css.titleCluster, children: [_jsxs("nav", { className: css.crumbs, "aria-label": t('session.hierarchy'), children: [ancestry.map((summary, index) => {
                                                const last = index === ancestry.length - 1;
                                                // The current crumb has no navigation, so it is plain text
                                                // rather than a disabled button: on darwin desktop a button
                                                // would subtract itself from the header's drag row (ui-web
                                                // base.css) and leave the title inert for dragging too.
                                                const title = last
                                                    ? (_jsx("span", { className: clsx(css.crumb, css.crumbCurrent), children: summary.displayTitle }))
                                                    : (_jsx("button", { type: "button", className: css.crumb, onClick: () => { open(summary.id); }, children: summary.displayTitle }));
                                                const lineage = last || summary.subagent;
                                                const lineageOwner = {
                                                    lineageSessionId: summary.id,
                                                    displayTitle: summary.displayTitle,
                                                    ...last ? {} : { openTitle: () => { open(summary.id); } },
                                                };
                                                return (_jsxs("span", { className: css.crumbSeg, ...getTriggerProps({ kind: 'session', value: { sessionId: summary.id, displayTitle: summary.displayTitle } }), children: [index > 0 && _jsx("span", { className: css.crumbSep, children: "/" }), lineage
                                                            ? summary.subagent
                                                                ? renderSlot('conversation.session.header.lineage', lineageOwner, { fallback: title })
                                                                : (_jsxs(_Fragment, { children: [title, renderSlot('conversation.session.header.lineage', lineageOwner, { fallback: null })] }))
                                                            : title] }, summary.id));
                                            }), ancestry.length === 0 && _jsx("span", { className: css.crumbCurrent, children: sessionId })] }), _jsx("div", { className: css.headerActions, children: renderSlot('conversation.session.header.actions', {}) })] }), _jsx("div", { className: css.headerUtilities, children: renderSlot('conversation.session.header.utilities', {}) })] })), _jsx("div", { className: css.headerCorner, "data-conversation-header-corner": "", children: renderSlot('conversation.session.header.corner', {}) })] }), showTabs && (_jsxs("div", { ref: tabRow, className: css.tabs, children: [_jsx("div", { className: css.viewTabs, role: "tablist", "data-conversation-tabs": "", children: visibleTabs.map(viewTab => (_jsx("button", { type: "button", role: "tab", "aria-selected": viewTab.id === active?.id, className: clsx(css.tab, viewTab.id === active?.id && css.tabActive), onClick: () => { selectView(viewTab.id); }, children: _jsx(ViewTabLabel, { label: viewTab.label }) }, viewTab.id))) }), hasEntryActions && (_jsx(ViewsMoreMenu, { label: t('session.moreViews'), current: currentInMore, entries: moreTabs.map(viewTab => ({
                            id: viewTab.id, label: viewTab.label,
                            pinned: !unpinned.includes(viewTab.id), active: viewTab.id === active?.id,
                        })), onOpen: selectView, onPinnedChange: setPinned, renderSlot: renderSlot }))] }))] }));
}
/**
 * Renders the active Session view inside the resident scrollport and keeps
 * the input draft mirrored while blank Hero chrome is visible.
 * @param props - Strict Session input/store, view ledger, and render shares.
 * @returns the active view area, or null while the Session remains blank.
 */
export function ConversationSession(props) {
    return _jsx(DefaultConversationViews, { ...props });
}
//# sourceMappingURL=ConversationSession.js.map