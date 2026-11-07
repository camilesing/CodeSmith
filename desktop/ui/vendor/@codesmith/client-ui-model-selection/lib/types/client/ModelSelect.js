import { jsx as _jsx, Fragment as _Fragment, jsxs as _jsxs } from "react/jsx-runtime";
/**
 * ModelSelect: the composer's named model seat (`conversation.input.model`).
 * Two-level selection per figma 496:26454's MenuDropdown: the root menu is
 * the Model / Effort row pair (label + current value + a right chevron),
 * each drilling into its own list — the provider-grouped model list over
 * the shared directory, and the effort levels. The trigger (313:14108's
 * ToggleButton) shows both: model name + effort in the caption tone.
 * Model catalogs above four entries show search, which retains focus while
 * ↑/↓ cycle the highlighted result; Enter and Tab accept it. Smaller model
 * catalogs, root panes, and effort panes move focus between rows. Escape and Shift+Tab leave a drilled pane first and otherwise close
 * back to the trigger. A drilled pane focuses the current effort or model
 * search field. Provider headings paint their background only while pinned
 * by scrolling. Clearing a query restores the full list and search focus.
 * Selecting restores trigger focus without a ring until the trigger loses focus
 * or the menu reopens. Mobile devices restore button focus when opening or
 * drilling; model search takes focus only after explicit user input.
 * Model names match a case-insensitive ordered subsequence
 * within each provider group, ranked by
 * prefix, alignment score, then catalog order. Returning to the root pane
 * hands focus back to the cell that opened it. Data and submission ride the
 * same per-session ModelDirectory as the /model popup; exact-model reasoning
 * metadata and the selected effort come from the Host rather than a
 * client-owned vocabulary. A rejected selection announces through the shared
 * transient Toast anchored to the composer card; the in-menu strip with
 * Retry remains the catalog-load surface. While the directory's pending
 * selection is unsettled, the trigger shows a spinner in place of its
 * chevron, and each row whose value that selection carries shows one in place
 * of its check mark.
 */
import { MenuGroup, MenuSurface, observeStickyMenuGroups, useAnchoredPosition } from '@codesmith/client-ui-primitives';
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, } from 'react';
import { createPortal } from 'react-dom';
import clsx from 'clsx';
import { IconCheckOutlineRegular, IconChevronDownOutlineRegular, IconChevronRightOutlineRegular, IconCloseFillRegular, IconDataOutlineRegular, IconWarningOutlineRegular, Input, isMobileDevice, rankByName, StateDot, Tag, Toast, } from '@codesmith/client-ui-primitives';
import css from './ModelSelect.module.css';
import { orderModelProviders } from "./provider-order.js";
/** Unplaced portal card: hidden but laid out at a fixed origin so offsetWidth/offsetHeight are real (Menu primitive's measure pass). */
const MEASURE_STYLE = { visibility: 'hidden', left: 0, top: 0 };
/**
 * Render the composer model seat.
 * @param props - owner share (locked) + injected face (shared directory
 * store/verbs) + the standard locale seat.
 * @returns the trigger and, while open, the two-level menu.
 */
export function ModelSelect({ locked, available, directory, load, select, resolveText, t, renderSlot }) {
    const state = useSyncExternalStore(fn => directory.subscribe(fn), () => directory.getSnapshot());
    const [open, setOpen] = useState(false);
    const [pane, setPane] = useState('root');
    const [query, setQuery] = useState('');
    const [highlightedIndex, setHighlightedIndex] = useState(null);
    const [selectionFocus, setSelectionFocus] = useState(false);
    // The in-menu error strip serves catalog loads (its Retry re-runs the
    // load); a rejected SELECTION announces through the transient toast
    // instead, so the strip renders only while the latest failure-capable
    // action was a load.
    const lastActionRef = useRef('load');
    const [toast, setToast] = useState(null);
    const toastSeq = useRef(0);
    const rootRef = useRef(null);
    const triggerRef = useRef(null);
    const searchRef = useRef(null);
    const menuRef = useRef(null);
    const groupsRef = useRef(null);
    const menuPos = useAnchoredPosition({
        open: open && available,
        anchorRef: triggerRef,
        panelRef: menuRef,
        side: 'top',
        align: 'end',
        gap: 8,
        margin: 12,
        layoutKey: pane,
    });
    const itemRefs = useRef([]);
    const id = useId();
    const accountProviderName = t('provider.account');
    const groups = useMemo(() => orderModelProviders(state.groups).map(group => ({
        ...group,
        provider: {
            id: group.id,
            name: group.id === 'codesmith-account' ? accountProviderName : group.name,
        },
    })), [state.groups, accountProviderName]);
    const choices = useMemo(() => groups.flatMap(group => group.models.map(model => ({
        group,
        model,
        selection: {
            provider: group.id,
            model: model.id,
            ...model.reasoning?.defaultEffort === undefined
                ? {}
                : { reasoningEffort: model.reasoning.defaultEffort },
        },
    }))), [groups]);
    const showSearch = choices.length > 4;
    const filteredGroups = useMemo(() => groups.map(group => ({
        ...group, models: rankByName(group.models, showSearch ? query.trim() : ''),
    })).filter(group => group.models.length > 0), [groups, query, showSearch]);
    const visibleModels = useMemo(() => filteredGroups.flatMap(group => group.models.map(model => ({
        provider: group.id, model: model.id,
    }))), [filteredGroups]);
    const currentVisibleIndex = visibleModels.findIndex(model => model.provider === state.current?.provider && model.model === state.current.model);
    const activeModelIndex = Math.min(highlightedIndex ?? Math.max(0, currentVisibleIndex), visibleModels.length - 1);
    const selectedIndex = state.current === null
        ? -1
        : choices.findIndex(c => c.selection.provider === state.current?.provider && c.selection.model === state.current.model);
    const currentChoice = choices[selectedIndex];
    const reasoning = currentChoice?.model.reasoning;
    const effectiveEffort = state.current?.reasoningEffort ?? reasoning?.defaultEffort;
    const effortLabel = currentChoice === undefined
        ? undefined
        : reasoning === undefined
            ? state.retainedEffort
            : effectiveEffort === undefined
                ? t('effort.providerDefault')
                : reasoning.efforts.find(level => level.id === effectiveEffort)?.name ?? effectiveEffort;
    const effortChoices = useMemo(() => reasoning === undefined
        ? []
        : [
            ...reasoning.defaultEffort === undefined
                ? [{ key: 'provider-default', effort: undefined, label: t('effort.providerDefault') }]
                : [],
            ...reasoning.efforts.map((effort) => ({
                key: `effort:${effort.id}`,
                effort: effort.id,
                label: effort.name,
            })),
        ], [reasoning, t]);
    const { pending } = state;
    const busy = pending !== null;
    const reload = () => {
        lastActionRef.current = 'load';
        load();
    };
    useEffect(() => {
        if (!open)
            return;
        const closeOutside = (event) => {
            // The portaled card is outside the trigger subtree; check both.
            if (rootRef.current?.contains(event.target) === true)
                return;
            if (menuRef.current?.contains(event.target) === true)
                return;
            setOpen(false);
        };
        document.addEventListener('mousedown', closeOutside);
        return () => { document.removeEventListener('mousedown', closeOutside); };
    }, [open]);
    useLayoutEffect(() => {
        if (!showSearch) {
            setQuery('');
            setHighlightedIndex(null);
        }
    }, [showSearch]);
    // Pane switches unmount the focused row; restore focus inside the menu so
    // keyboard navigation remains available.
    const paneFocus = useRef(null);
    const previousShowSearch = useRef(showSearch);
    useEffect(() => {
        const changedSearchMode = previousShowSearch.current !== showSearch;
        previousShowSearch.current = showSearch;
        const intent = paneFocus.current ?? (changedSearchMode && pane === 'model' ? 'drill' : null);
        paneFocus.current = null;
        if (!open || intent === null)
            return;
        if (intent === 'drill') {
            if (pane === 'model' && showSearch && !isMobileDevice()) {
                searchRef.current?.focus();
                return;
            }
            // The checked row is the value in use; a pane without one opens on its
            // first row.
            const checked = menuRef.current?.querySelector('[role="menuitemradio"][aria-checked="true"]:not([disabled])');
            const target = checked ?? itemRefs.current.find(item => item !== null && !item.disabled);
            (target ?? triggerRef.current)?.focus();
            return;
        }
        const cell = itemRefs.current[intent === 'effort' ? 1 : 0];
        (cell !== null && cell !== undefined && !cell.disabled ? cell : triggerRef.current)?.focus();
    }, [open, pane, showSearch]);
    useEffect(() => {
        const viewport = groupsRef.current;
        if (viewport === null)
            return;
        return observeStickyMenuGroups(viewport);
    }, [available, open, pane, filteredGroups]);
    useLayoutEffect(() => {
        if (open && pane === 'model' && activeModelIndex >= 0) {
            itemRefs.current[activeModelIndex]?.scrollIntoView({ block: 'nearest' });
        }
    }, [open, pane, activeModelIndex, visibleModels]);
    if (!available)
        return null;
    const show = () => {
        setSelectionFocus(false);
        triggerRef.current?.focus();
        setQuery('');
        setHighlightedIndex(null);
        if (currentChoice === undefined)
            paneFocus.current = 'drill';
        setPane(currentChoice === undefined ? 'model' : 'root');
        setOpen(true);
        reload();
    };
    const changeQuery = (next) => {
        setQuery(next);
        setHighlightedIndex(0);
    };
    const close = (restoreFocus = false) => {
        setOpen(false);
        setPane('root');
        if (restoreFocus)
            queueMicrotask(() => { triggerRef.current?.focus(); });
    };
    const closeAfterSelection = () => {
        setSelectionFocus(true);
        close(true);
    };
    const drill = (next) => {
        setQuery('');
        setHighlightedIndex(null);
        paneFocus.current = 'drill';
        setPane(next);
    };
    /** Leave a drilled pane for the root one, handing the keyboard back to its cell. */
    const back = (from) => {
        paneFocus.current = from;
        setPane('root');
    };
    const moveFocus = (offset) => {
        const items = Array.from(menuRef.current?.querySelectorAll('button[role="menuitem"]:not([disabled]), button[role="menuitemradio"]:not([disabled]), button[role="switch"]:not([disabled])') ?? []);
        if (items.length === 0)
            return;
        const active = items.findIndex(item => item === document.activeElement);
        // Focus outside the rows (the trigger, which keeps it while the menu
        // opens) enters at the end the step comes from: the first row forward,
        // the last row backward.
        const next = active === -1
            ? (offset > 0 ? 0 : items.length - 1)
            : (active + offset + items.length) % items.length;
        items[next]?.focus();
    };
    const onRootKeyDown = (event) => {
        if (event.nativeEvent.isComposing)
            return;
        if (event.key === 'Escape' && open) {
            event.preventDefault();
            // Escape backs out of a drilled pane first, then closes.
            if (pane !== 'root' && currentChoice !== undefined)
                back(pane);
            else
                close(true);
            return;
        }
        if (!open)
            return;
        if (pane === 'model' && showSearch && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
            event.preventDefault();
            if (!busy && visibleModels.length > 0) {
                const direction = event.key === 'ArrowDown' ? 1 : -1;
                setHighlightedIndex((activeModelIndex + direction + visibleModels.length) % visibleModels.length);
                searchRef.current?.focus();
            }
            return;
        }
        if (pane === 'model' && showSearch && event.target instanceof HTMLInputElement
            && (event.key === 'Enter' || (event.key === 'Tab' && !event.shiftKey))) {
            if (event.key === 'Tab' && visibleModels.length === 0)
                return;
            event.preventDefault();
            const highlighted = visibleModels[activeModelIndex];
            if (!busy && highlighted !== undefined)
                choose(highlighted);
            return;
        }
        // Tab settles like Enter and Shift+Tab leaves like Escape, so the menu's
        // keys mean what they mean in the composer. Both are consumed: the card
        // keeps the browser's focus traversal out while it is open.
        if (event.key === 'Tab') {
            if (event.shiftKey) {
                event.preventDefault();
                if (pane !== 'root' && currentChoice !== undefined)
                    back(pane);
                else
                    close(true);
                return;
            }
            // Settling activates the row the keyboard is on; with focus still on the
            // trigger, Tab enters the menu at the value in use instead. Any other
            // control inside the card (a retry button) keeps the browser's traversal,
            // so the keystroke stays unconsumed there.
            const focused = document.activeElement;
            const rows = Array.from(menuRef.current?.querySelectorAll('button[role="menuitem"]:not([disabled]), button[role="menuitemradio"]:not([disabled]), button[role="switch"]:not([disabled])') ?? []);
            if (focused instanceof HTMLButtonElement && rows.includes(focused)) {
                event.preventDefault();
                focused.click();
                return;
            }
            if (focused !== triggerRef.current)
                return;
            event.preventDefault();
            if (pane === 'model' && showSearch) {
                setHighlightedIndex(null);
                searchRef.current?.focus();
                return;
            }
            const checked = menuRef.current?.querySelector('[role="menuitemradio"][aria-checked="true"]:not([disabled])');
            (checked ?? rows.find(item => !item.disabled))?.focus();
            return;
        }
        if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            moveFocus(event.key === 'ArrowDown' ? 1 : -1);
        }
    };
    const onBlur = (event) => {
        if (event.relatedTarget instanceof Node && (rootRef.current?.contains(event.relatedTarget) === true
            || menuRef.current?.contains(event.relatedTarget) === true))
            return;
        close();
    };
    const settleSelection = (result) => {
        if (result === undefined)
            return;
        if (result.ok) {
            if (rootRef.current !== null)
                closeAfterSelection();
            return;
        }
        const { error } = result;
        toastSeq.current += 1;
        setToast({
            seq: toastSeq.current,
            text: error.code === 'session/writer-held'
                ? t('error.sessionInUse')
                : t('error.action', { message: `${error.code}: ${error.message}` }),
        });
    };
    const submit = (selection) => {
        lastActionRef.current = 'select';
        // Disabled option rows cannot retain focus while a selection is pending.
        setSelectionFocus(true);
        triggerRef.current?.focus();
        void select(selection).then(settleSelection);
    };
    const choose = (selection) => {
        if (state.current?.provider === selection.provider && state.current.model === selection.model) {
            closeAfterSelection();
            return;
        }
        submit(selection);
    };
    const chooseEffort = (effort) => {
        if (state.current === null)
            return;
        if (effectiveEffort === effort) {
            closeAfterSelection();
            return;
        }
        const selection = {
            provider: state.current.provider,
            model: state.current.model,
            ...effort === undefined ? {} : { reasoningEffort: effort },
        };
        submit(selection);
    };
    const waiting = currentChoice === undefined && state.status === 'loading';
    const modelLabel = waiting
        ? t('trigger.loading')
        : currentChoice?.model.name
            ?? t('trigger.fallback');
    const triggerLabel = effortLabel === undefined ? modelLabel : `${modelLabel} · ${effortLabel}`;
    const triggerAria = waiting
        ? t('trigger.loading')
        : currentChoice === undefined
            ? t('trigger.selectAria')
            : effortLabel === undefined
                ? t('trigger.aria', { model: modelLabel })
                : t('trigger.ariaEffort', { model: modelLabel, effort: effortLabel });
    const renderModelLabel = (provider, model, placement, selected) => (_jsx("span", { "data-provider-id": provider.id, "data-model-id": model.id, children: renderSlot('conversation.input.model.label', {
            provider, model, placement, selected,
        }, {
            fallback: model.badge === undefined ? model.name : (_jsxs("span", { className: css.badgedName, children: [placement === 'trigger' ? _jsxs(_Fragment, { children: [_jsx(Tag, { className: css.badge, tone: model.badge.tone, children: resolveText(model.badge.text) }), ' '] }) : null, _jsx("span", { className: css.badgedNameText, children: model.name }), placement === 'trigger' ? null : _jsxs(_Fragment, { children: [' ', _jsx(Tag, { className: css.badge, tone: model.badge.tone, children: resolveText(model.badge.text) })] })] })),
        }) }));
    let itemIndex = 0;
    let modelIndex = 0;
    const itemRef = () => {
        const at = itemIndex++;
        return (node) => { itemRefs.current[at] = node; };
    };
    return (_jsxs("div", { ref: rootRef, className: css.root, onKeyDown: onRootKeyDown, onBlur: onBlur, onMouseDown: (event) => {
            // WebKit blurs a focused row before click unless the button's mousedown keeps focus.
            if (event.target instanceof Element && event.target.closest('button') !== null)
                event.preventDefault();
        }, children: [_jsxs("button", { ref: triggerRef, type: "button", className: css.trigger, "aria-label": triggerAria, "aria-haspopup": "menu", "aria-expanded": open, "aria-controls": open ? `${id}-menu` : undefined, title: triggerLabel, "aria-busy": busy, "data-selection-focus": selectionFocus ? '' : undefined, onBlur: () => { setSelectionFocus(false); }, disabled: locked, onClick: () => {
                    if (open) {
                        close(true);
                    }
                    else {
                        show();
                    }
                }, children: [_jsx(IconDataOutlineRegular, { className: css.triggerIcon, size: 16 }), _jsx("span", { className: css.triggerLabel, children: currentChoice === undefined ? modelLabel : renderModelLabel(currentChoice.group.provider, currentChoice.model, 'trigger', true) }), effortLabel !== undefined && _jsx("span", { className: css.triggerEffort, children: effortLabel }), busy
                        ? _jsx(StateDot, { state: "ongoing" })
                        : _jsx(IconChevronDownOutlineRegular, { className: clsx(css.chevron, open && css.chevronOpen) })] }), open && createPortal(_jsxs(MenuSurface, { ref: menuRef, id: `${id}-menu`, className: css.menu, style: menuPos ?? MEASURE_STYLE, role: pane === 'model' ? 'group' : 'menu', "aria-label": t('menu.aria'), "aria-busy": state.status === 'loading' || busy, children: [pane === 'root' && (_jsxs(_Fragment, { children: [_jsxs("button", { ref: itemRef(), type: "button", role: "menuitem", className: css.cell, onClick: () => { drill('model'); }, children: [_jsx("span", { className: css.cellLabel, children: t('menu.model') }), _jsx("span", { className: css.cellValue, children: currentChoice === undefined ? modelLabel : renderModelLabel(currentChoice.group.provider, currentChoice.model, 'menu', true) }), _jsx(IconChevronRightOutlineRegular, { className: css.cellChevron })] }), reasoning !== undefined && (_jsxs("button", { ref: itemRef(), type: "button", role: "menuitem", className: css.cell, onClick: () => { drill('effort'); }, children: [_jsx("span", { className: css.cellLabel, children: t('menu.effort') }), _jsx("span", { className: css.cellValue, children: effortLabel }), _jsx(IconChevronRightOutlineRegular, { className: css.cellChevron })] })), renderSlot('conversation.input.model.options', {
                                locked: locked || busy,
                                dismiss: () => {
                                    close();
                                    triggerRef.current?.focus();
                                },
                                reportFailure: (message) => { setToast({ seq: ++toastSeq.current, text: message }); },
                            })] })), pane === 'model' && (_jsxs(_Fragment, { children: [showSearch && _jsxs("div", { className: css.searchRow, children: [_jsx(Input, { ref: searchRef, className: clsx(css.search, query !== '' && css.searchWithQuery), type: "text", role: "searchbox", "aria-label": t('search.placeholder'), "aria-controls": `${id}-models`, "aria-activedescendant": activeModelIndex < 0 ? undefined : `${id}-model-${activeModelIndex}`, placeholder: t('search.placeholder'), value: query, readOnly: busy, onChange: (event) => { changeQuery(event.target.value); } }), query !== '' && (_jsx("button", { type: "button", className: css.searchClear, "aria-label": t('search.clear'), disabled: busy, onClick: () => {
                                            changeQuery('');
                                            searchRef.current?.focus();
                                        }, children: _jsx(IconCloseFillRegular, {}) }))] }), state.status === 'loading' && (_jsx("div", { className: css.status, children: t('status.loading') })), state.error !== null && lastActionRef.current === 'load' && (_jsxs("div", { className: css.error, children: [_jsx("span", { children: t('error.action', { message: state.error }) }), _jsx("button", { type: "button", className: css.retry, onClick: reload, children: t('retry') })] })), state.failures.map(failure => (_jsxs("div", { className: css.warning, children: [_jsx("span", { children: t('warning.groupLoad', { name: failure.id === 'codesmith-account' ? t('provider.account') : failure.name, message: failure.message }) }), _jsx("button", { type: "button", className: css.retry, onClick: reload, children: t('retry') })] }, failure.id))), _jsx("div", { ref: groupsRef, id: `${id}-models`, className: clsx(css.groups, 'scrollable'), role: "menu", "aria-label": t('menu.model'), hidden: filteredGroups.length === 0, children: filteredGroups.map((group) => {
                                    return (_jsx(MenuGroup, { label: group.provider.name, children: group.models.map((model) => {
                                            const index = modelIndex++;
                                            const selected = state.current?.provider === group.id && state.current.model === model.id;
                                            return (_jsxs("button", { ref: itemRef(), type: "button", role: "menuitemradio", "aria-checked": selected, id: `${id}-model-${index}`, tabIndex: showSearch ? -1 : 0, onFocus: () => { setHighlightedIndex(index); }, "data-highlighted": index === activeModelIndex ? '' : undefined, className: clsx(css.option, css.modelOption, selected && css.selected, index === activeModelIndex && css.optionActive), onMouseMove: busy || index === activeModelIndex ? undefined : () => {
                                                    if (showSearch)
                                                        setHighlightedIndex(index);
                                                    else
                                                        itemRefs.current[index]?.focus();
                                                }, title: model.name, disabled: busy, onClick: () => { choose({ provider: group.id, model: model.id }); }, children: [_jsx("span", { className: css.optionCopy, children: _jsx("span", { className: css.modelName, children: renderModelLabel(group.provider, model, 'option', selected) }) }), _jsx("span", { className: css.check, children: pending?.provider === group.id && pending.model === model.id
                                                            ? _jsx(StateDot, { state: "ongoing" })
                                                            : selected ? _jsx(IconCheckOutlineRegular, {}) : null })] }, model.id));
                                        }) }, group.id));
                                }) }), state.status === 'ready' && filteredGroups.length === 0 && (_jsx("div", { className: css.empty, role: "status", children: t(choices.length === 0 ? 'empty.models' : 'search.empty') }))] })), pane === 'effort' && (_jsxs(_Fragment, { children: [state.error !== null && lastActionRef.current === 'load' && (_jsxs("div", { className: css.error, children: [_jsx("span", { children: t('error.action', { message: state.error }) }), _jsx("button", { type: "button", className: css.retry, onClick: reload, children: t('action.reload') })] })), _jsx("div", { className: clsx(css.groups, 'scrollable'), children: effortChoices.length === 0
                                    ? _jsx("div", { className: css.empty, children: t('empty.efforts') })
                                    : effortChoices.map(level => (_jsxs("button", { ref: itemRef(), type: "button", role: "menuitemradio", "aria-checked": effectiveEffort === level.effort, className: clsx(css.option, effectiveEffort === level.effort && css.selected), disabled: busy, onClick: () => { chooseEffort(level.effort); }, children: [_jsx("span", { className: css.optionCopy, children: _jsx("span", { className: css.modelName, children: level.label }) }), _jsx("span", { className: css.check, children: pending !== null && pending.provider === state.current?.provider
                                                    && pending.model === state.current.model && pending.reasoningEffort === level.effort
                                                    ? _jsx(StateDot, { state: "ongoing" })
                                                    : effectiveEffort === level.effort ? _jsx(IconCheckOutlineRegular, {}) : null })] }, level.key))) })] }))] }), document.body), toast !== null && (_jsx(Toast, { text: toast.text, icon: _jsx(IconWarningOutlineRegular, {}), anchor: rootRef.current?.closest('[data-composer-card]') ?? null, onDone: () => { setToast(null); } }, toast.seq))] }));
}
//# sourceMappingURL=ModelSelect.js.map