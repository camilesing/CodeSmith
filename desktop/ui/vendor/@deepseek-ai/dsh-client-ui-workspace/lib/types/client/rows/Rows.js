import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
/**
 * Workspace browser tree row components (figma Cell set 14:3080): pure presentational —
 * all data and callbacks arrive via props. Hover swaps (folder->chevron,
 * time->ellipsis, action buttons) are CSS-only, and a session row's clipped
 * title marquees programmatically while the row is hovered. Workspace row
 * menus share captured Workspace actions with right-click. A Session row's "..." menu and
 * its hover buttons are the `sidebar.workspaces.session.menu.item` and
 * `sidebar.workspaces.session.row.action` lists, rendered through the
 * browser's `renderSlot` with the menu's open state as the occurrence's hook
 * context; this package's own actions are entries like any plugin's. The
 * session and workspace hover cards are suppressed while a menu is open.
 */
import { useEffect, useMemo, useRef } from 'react';
import clsx from 'clsx';
import { HoverCard, IconArchiveOutlineRegular, IconEllipsisOutlineRegular, IconFolderCloseRegular, IconFolderOpenRegular, IconNewChatOutlineRegular, IconPinFillRegular, IconTriangleRightFillRegular, IconUnarchiveOutlineRegular, relativeTime, StateDot, Tooltip, useRowDragPreview, } from '@deepseek-ai/dsh-client-ui-primitives';
import { abbreviateHomePath } from '@deepseek-ai/dsh-util-workspace-path';
import css from './Rows.module.css';
/** Sidebar-owned layout values needed by row copies mounted outside the sidebar. */
const DRAG_PREVIEW_PROPERTIES = [
    '--dsh-sidebar-row-padding',
    '--dsh-sidebar-label-gap',
    '--dsh-sidebar-paint-inset',
];
/** Row display title: blank rows show the localized New Session label. */
function displayTitle(node, t) {
    return node.blank ? t('session.new') : (node.title || t('session.untitled'));
}
/* Overflow this small hides no meaningful tail; scrolling for it reads as an
   accidental jitter, so the title stays put. */
const MIN_TITLE_REVEAL_PX = 8;
/* Marquee travel speed: slow enough to read the text as it passes. */
const TITLE_MARQUEE_PX_PER_MS = 0.03;
/**
 * Place the title's scroll position and publish the stylesheet's fade-mask
 * hooks: `data-scrolled` while the title has left its start (left fade) and
 * `data-clipped` while text remains beyond the right edge (right fade).
 * @param title - the row's clipping title element.
 * @param left - scroll offset in CSS pixels.
 * @param range - the title's maximum scroll offset in CSS pixels.
 */
function placeTitle(title, left, range) {
    // jsdom implements no scrollTo; the lane's direct assignment is instant there
    // anyway, so both paths land on the same position.
    if (typeof title.scrollTo === 'function')
        title.scrollTo({ left, behavior: 'instant' });
    else
        title.scrollLeft = left;
    if (left > 0)
        title.dataset.scrolled = '';
    else
        delete title.dataset.scrolled;
    if (left < range)
        title.dataset.clipped = '';
    else
        delete title.dataset.clipped;
}
/**
 * Return the title to its resting state: scrolled to the start with both fade
 * masks off, so the resting ellipsis renders at full strength.
 * @param title - the row's clipping title element.
 */
function restTitle(title) {
    if (typeof title.scrollTo === 'function')
        title.scrollTo({ left: 0, behavior: 'instant' });
    else
        title.scrollLeft = 0;
    delete title.dataset.scrolled;
    delete title.dataset.clipped;
}
/**
 * Marquee a title wider than its one-line cell while its row is hovered: the
 * title clips its own text, so entering crawls it at a constant speed until the
 * far edge (a fork's incremented title, for example) is in view, then rests
 * there under the pointer. Overflow of at most {@link MIN_TITLE_REVEAL_PX}
 * stays put — a barely-clipped title moving a few pixels reads as jitter, not a
 * reveal. Leaving returns the title to the start in one step, because the
 * resting ellipsis and the narrowed cell would otherwise meet the text while it
 * travelled back. Reduced motion jumps to the far edge instead of crawling.
 * @param title - ref to the row's clipping title element.
 * @returns stable pointer enter/leave handlers for the row.
 */
function useTitleMarquee(title) {
    const frame = useRef(0);
    const revealing = useRef(false);
    useEffect(() => () => { cancelAnimationFrame(frame.current); }, []);
    return useMemo(() => ({
        enter: (event) => {
            if (event?.pointerType === 'touch')
                return;
            /* v8 ignore next -- defensive: the title span renders unconditionally. */
            if (title.current === null)
                return;
            if (revealing.current)
                return;
            revealing.current = true;
            const element = title.current;
            const range = element.scrollWidth - element.clientWidth;
            if (range <= MIN_TITLE_REVEAL_PX)
                return;
            if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
                placeTitle(element, range, range);
                return;
            }
            cancelAnimationFrame(frame.current);
            let previous;
            let position = 0;
            const step = (now) => {
                position += previous === undefined ? 0 : (now - previous) * TITLE_MARQUEE_PX_PER_MS;
                previous = now;
                placeTitle(element, Math.min(position, range), range);
                if (position < range)
                    frame.current = requestAnimationFrame(step);
            };
            frame.current = requestAnimationFrame(step);
        },
        leave: () => {
            revealing.current = false;
            cancelAnimationFrame(frame.current);
            /* v8 ignore next -- defensive: the title span renders unconditionally. */
            if (title.current === null)
                return;
            restTitle(title.current);
        },
    }), [title]);
}
/** Localized compact relative time ("刚刚"/"5分钟" in zh, "now"/"5min" in en). */
function timeLabel(updatedAt, now, t) {
    const { unit, n } = relativeTime(updatedAt, now);
    return unit === 'now' ? t('time.now') : t(`time.${unit}`, { n });
}
/** Hover-card variant: distances wrap in the ago template; the now bucket stays bare (no "now ago"). */
function hoverTimeLabel(updatedAt, now, t) {
    const { unit, n } = relativeTime(updatedAt, now);
    return unit === 'now' ? t('time.now') : t('time.ago', { t: t(`time.${unit}`, { n }) });
}
/**
 * Absolute creation time through the dictionary's date template (the message
 * clock pattern): `toLocaleString` would follow the browser language, not the
 * app locale, and produce mixed-language text after a switch.
 */
function createdLabel(createdAt, t) {
    const d = new Date(createdAt);
    const pad2 = (v) => String(v).padStart(2, '0');
    const date = t('date.ymd', { y: d.getFullYear(), m: d.getMonth() + 1, d: d.getDate() });
    return t('hover.created', { time: `${date} ${pad2(d.getHours())}:${pad2(d.getMinutes())}` });
}
/** Hover-card body: workspace title, display directory path, absolute creation time. */
function WorkspaceHoverContent({ label, cwd, createdAt, t }) {
    return (_jsxs("div", { className: css.hoverContent, children: [_jsx("div", { className: css.hoverTitle, children: label }), _jsx("div", { className: css.hoverPath, children: cwd }), _jsx("div", { className: css.hoverTime, children: createdLabel(createdAt, t) })] }));
}
/** Pointer-position half of a row (insert line above or below). */
function rowHalf(e) {
    const rect = e.currentTarget.getBoundingClientRect();
    return e.clientY < rect.top + rect.height / 2 ? 'before' : 'after';
}
/**
 * Project (workspace) header row: folder + title;
 * hover reveals the chevron and create button, and dwelling on a real
 * Workspace shows its hover card (the ungrouped bucket has none).
 * `containsCurrent` arrives on the node (derivation fact, no renderer scan).
 * Create and drag follow the Workspace access snapshot. Menu providers own
 * their access checks; the ungrouped bucket has no Workspace menu.
 * @param props.group - derived group node.
 * @param props.containsCurrentDescendant - highlight an ancestor even when its subtree is collapsed.
 * @param props.onToggle - expand/collapse the group.
 * @param props.onCreate - start a frontend Session inside this Workspace.
 * @param props.drag - optional workspace-row drag wiring.
 * @param props.home - host account home for POSIX hover-path abbreviation.
 * @param props.t - the browser root's locale seat.
 * @returns the row element.
 */
export function ProjectRowItem(props) {
    const workspaceId = props.group.workspaceId;
    return workspaceId === undefined
        ? _jsx(UngroupedProjectRow, { ...props })
        : _jsx(WorkspaceProjectRow, { ...props, workspaceId: workspaceId });
}
/** The ungrouped bucket's header: no Workspace resource, so starting a Session there asks the global resource. */
function UngroupedProjectRow(props) {
    const create = props.useRemoteAccess('session/create');
    return _jsx(ProjectRow, { ...props, access: { create, reorder: false } });
}
/** A real Workspace's header: every action asks that Workspace's resource. */
function WorkspaceProjectRow({ workspaceId, ...props }) {
    const { useWorkspaceAccess } = props;
    const access = {
        create: useWorkspaceAccess(workspaceId, 'session/create'),
        reorder: useWorkspaceAccess(workspaceId, 'workspace/insertBefore'),
    };
    return _jsx(ProjectRow, { ...props, access: access });
}
function ProjectRow({ group, containsCurrentDescendant = false, onToggle, onCreate, drag: offeredDrag, home, newShortcut, t, access, openWorkspaceMenu, useContextMenuTarget, useContextMenu, }) {
    const { getTriggerProps } = useContextMenu();
    const drag = access.reorder ? offeredDrag : undefined;
    const row = group;
    // The ungrouped bucket has no workspace title: its label is dictionary copy.
    const label = row.workspaceId === undefined ? t('group.ungrouped') : row.label;
    const active = containsCurrentDescendant || (group.expanded && group.containsCurrent);
    const menuOpen = useContextMenuTarget(target => target?.kind === 'workspace' && target.value.workspaceId === row.workspaceId);
    const dragPreview = useRowDragPreview(css.dragPreview, DRAG_PREVIEW_PROPERTIES);
    const workspaceMenuTarget = row.workspaceId !== undefined && row.cwd !== undefined
        ? { workspaceId: row.workspaceId, path: row.cwd } : undefined;
    const ownRow = (_jsxs("div", { className: clsx(css.projectRow, menuOpen && css.menuOpen), "data-row-key": `workspace:${group.key}`, role: "treeitem", "aria-expanded": row.expanded, onClick: onToggle, ...getTriggerProps(workspaceMenuTarget === undefined ? undefined : { kind: 'workspace', value: { ...workspaceMenuTarget, displayTitle: label } }), draggable: drag !== undefined, onDragStart: drag === undefined
            ? undefined
            : (e) => {
                e.dataTransfer.effectAllowed = 'move';
                e.dataTransfer.setData('text/plain', row.key);
                drag.start();
                dragPreview.start(e);
            }, onDragEnd: () => { if (dragPreview.clear())
            drag?.end(); }, children: [_jsx("span", { className: clsx(css.slot, css.folder, active && css.folderActive), children: row.expanded ? _jsx(IconFolderOpenRegular, {}) : _jsx(IconFolderCloseRegular, {}) }), _jsx("span", { className: clsx(css.slot, css.chevron), children: _jsx(IconTriangleRightFillRegular, { className: clsx(css.arrow, row.expanded && css.arrowOpen) }) }), _jsx("span", { className: css.projectText, children: _jsx("span", { className: css.title, children: label }) }), _jsxs("span", { className: css.rowActions, children: [workspaceMenuTarget !== undefined && (_jsx("button", { type: "button", className: css.iconButton, "data-context-menu-trigger": true, "aria-label": t('actions.workspace.aria', { name: label }), "aria-haspopup": "menu", "aria-expanded": menuOpen, onClick: (event) => {
                            event.stopPropagation();
                            const rect = event.currentTarget.getBoundingClientRect();
                            openWorkspaceMenu(workspaceMenuTarget.workspaceId, workspaceMenuTarget.path, label, { x: rect.left, y: rect.bottom }, 'toggle');
                        }, children: _jsx(IconEllipsisOutlineRegular, {}) })), access.create && (_jsx(Tooltip, { label: t('actions.newSession'), shortcutKeys: newShortcut?.keys, side: "bottom", align: "end", delayMs: 500, portal: true, children: _jsx("button", { type: "button", className: css.iconButton, "aria-keyshortcuts": newShortcut?.aria, "aria-label": t('actions.newSession.aria', { name: label }), onClick: (e) => { e.stopPropagation(); onCreate(); }, children: _jsx(IconNewChatOutlineRegular, { size: 14 }) }) }))] })] }));
    // The ungrouped bucket has no backing Workspace: no card to show.
    if (row.createdAt === undefined)
        return ownRow;
    return (_jsx(HoverCard, { anchor: ownRow, content: _jsx(WorkspaceHoverContent, { label: row.label, cwd: row.cwd === undefined ? undefined : abbreviateHomePath(row.cwd, home), createdAt: row.createdAt, t: t }), openDelayMs: 800, grouped: true, disabled: menuOpen, copyText: row.cwd, copyLabel: t('copy'), copiedLabel: t('hover.copied') }));
}
/* v8 ignore next 3 -- closed-union backstop; only reached if the status is forged */
function assertNever(value) {
    throw new Error(`unknown pending interaction: ${String(value)}`);
}
/**
 * Session status presentation; pending interaction is primary and live activity
 * outranks completion reminders.
 */
function sessionStatuses(node, t) {
    const subagents = node.runningSubagentCount === 0
        ? undefined
        : {
            state: 'ongoing',
            label: t(node.runningSubagentCount === 1
                ? 'status.subagentsRunning.one'
                : 'status.subagentsRunning.other', { n: node.runningSubagentCount }),
        };
    let pending;
    switch (node.pendingInteraction) {
        case 'approval':
            pending = {
                state: 'warning',
                label: t('status.waitingApproval'),
                trailingLabel: t('status.compact.approval'),
            };
            break;
        case 'plan-review':
            pending = {
                state: 'warning',
                label: t('status.planReview'),
                trailingLabel: t('status.compact.planReview'),
            };
            break;
        case 'question':
            pending = {
                state: 'warning',
                label: t('status.waitingAnswer'),
                trailingLabel: t('status.compact.answer'),
            };
            break;
        case undefined: break;
        /* v8 ignore next -- closed PendingInteractionStatus union */
        default: return assertNever(node.pendingInteraction);
    }
    if (pending !== undefined)
        return subagents === undefined ? [pending] : [pending, subagents];
    if (node.running) {
        const primary = { state: 'ongoing', label: t('status.running') };
        return subagents === undefined ? [primary] : [primary, subagents];
    }
    if (subagents !== undefined)
        return [subagents];
    if (node.completed)
        return [{ state: 'done', label: t('status.completed') }];
    return [{ state: 'idle', label: t('status.idle') }];
}
/** Primary status dot plus every status's screen-reader label, shared by the search and session rows. */
function SessionStatusDots({ statuses }) {
    return (_jsxs(_Fragment, { children: [_jsx(StateDot, { state: statuses[0].state }), statuses.map(status => (_jsx("span", { className: css.visuallyHidden, children: status.label }, status.label)))] }));
}
/** Non-interactive pinned-row marker; the enclosing row remains the only action. */
function PinnedIndicator({ t }) {
    const label = t('row.pinned');
    return (_jsx("span", { className: css.pinIndicator, role: "img", "aria-label": label, title: label, children: _jsx(IconPinFillRegular, { size: 14 }) }));
}
/**
 * Hover-card body: full title, display working directory, relative time, the Session's own scheduled-task
 * section, and every relevant live status. The task section sits above the
 * status lines so they stay the card's trailing status line.
 */
function SessionHoverContent({ node, cwd, now, renderSlot, t }) {
    // On archived rows the archived line already says the session is inactive,
    // so resting statuses (idle/completed) drop; live activity still shows.
    const statuses = sessionStatuses(node, t)
        .filter(status => !(node.archived && (status.state === 'done' || status.state === 'idle')));
    return (_jsxs("div", { className: css.hoverContent, children: [_jsx("div", { className: css.hoverTitle, children: displayTitle(node, t) }), cwd !== undefined && _jsx("div", { className: css.hoverPath, children: cwd }), !node.blank && _jsx("div", { className: css.hoverTime, children: hoverTimeLabel(node.updatedAt, now, t) }), renderSlot('sidebar.session.row.hover', { sessionId: node.id }), statuses.map(status => (_jsxs("div", { className: css.hoverStatus, children: [_jsx(StateDot, { state: status.state }), _jsx("span", { children: status.label })] }, status.label))), node.archived && (_jsxs("div", { className: clsx(css.hoverStatus, css.hoverArchived), children: [_jsx(IconArchiveOutlineRegular, { size: 14 }), _jsx("span", { children: t('row.archived') })] }))] }));
}
/**
 * One flat search result: title, Workspace context, and optional content
 * excerpt. Search navigation opens the session only; it does not address an
 * event inside the conversation. Archived rows carry a hover unarchive
 * button, because search is where the filter surfaces them for recovery.
 * @param props.result - merged local/content search row.
 * @param props.currentId - selected session id.
 * @param props.onOpen - open the selected session.
 * @param props.onUnarchive - unarchive an archived result row.
 * @param props.useSessionAccess - the browser root's Session access Hook; the restore control
 * appears only while the snapshot allows `workspace/unarchiveSession` for that Session.
 * @param props.t - Workspace-browser translation seat.
 * @returns the result row.
 */
export function SearchResultItem({ result, currentId, onOpen, onUnarchive, useSessionAccess, t, }) {
    const selected = result.id === currentId;
    const canUnarchive = useSessionAccess(result.id, 'workspace/unarchiveSession');
    const statuses = sessionStatuses(result, t);
    const primaryStatus = statuses[0];
    return (_jsxs("div", { className: clsx(css.searchResultRow, selected && css.selected, result.archived && css.archived), role: "treeitem", "aria-selected": selected, "aria-description": result.archived ? t('toast.archivedNotOpenable') : undefined, onClick: () => { onOpen(result.id); }, children: [_jsxs("span", { className: css.searchResultHeading, children: [_jsx("span", { className: css.slot, children: !result.archived && primaryStatus.state !== 'idle' && (_jsx(SessionStatusDots, { statuses: statuses })) }), _jsx("span", { className: css.searchResultTitle, children: result.title || t('session.untitled') }), result.archived && canUnarchive && (_jsx("span", { className: css.rowActions, children: _jsx(Tooltip, { label: t('actions.unarchive'), side: "bottom", align: "end", delayMs: 500, portal: true, children: _jsx("button", { type: "button", className: css.iconButton, "aria-label": t('menu.unarchiveSession'), onClick: (e) => { e.stopPropagation(); onUnarchive(result.id); }, children: _jsx(IconUnarchiveOutlineRegular, { size: 14 }) }) }) }))] }), _jsxs("span", { className: css.searchResultMeta, children: [_jsx("span", { className: css.searchResultWorkspace, children: result.workspace || t('group.ungrouped') }), result.snippet !== undefined && (_jsx("span", { className: css.searchResultSnippet, children: result.snippet }))] })] }));
}
/**
 * One top-level 32px session row: leading 16px cell (status dot, or the
 * leading seat while the row's primary state is idle), title, relative time or
 * compact pending label, and the row actions menu. A row that owns a state dot
 * keeps that cell and renders no seat, so an ambient automation mark never
 * appears beside the row's own state dot. An archived row keeps the cell blank:
 * neither marker renders there, and its live status stays on the hover card.
 * The row accepts Tab focus and opens with unmodified Enter or Space; descendant controls
 * handle their own activation without opening the row.
 * @param props.node - derived session node.
 * @param props.currentId - selected session id (row highlight).
 * @param props.now - epoch ms for relative-time formatting.
 * @param props.onOpen - open a session by id.
 * @param props.onRenameRequest - open the rename dialog from a title double-click (id + current title);
 * called only after a single press on the title since the window last lost focus.
 * @param props.renderSlot - child-seat renderer for the row's action lists
 * (`sidebar.workspaces.session.menu.item` / `sidebar.workspaces.session.row.action`),
 * its leading and trailing decorations, and its hover-card section.
 * @param props.onReveal - scroll this row into view after search navigation, then acknowledge it.
 * @param props.drag - optional row-drag target wiring; blank rows, and rows whose access excludes
 * `workspace/insertSessionBefore`, cannot start a drag.
 * @param props.home - host account home for POSIX hover-path abbreviation.
 * @param props.useSessionAccess - the browser root's Session access Hook; the title double-click
 * renames only while it allows `session/rename`.
 * @param props.t - the browser root's locale seat.
 * @returns the session row.
 */
export function SessionNodeItem({ node, currentId, now, onOpen, onRenameRequest, renderSlot, openSessionMenu, useContextMenuTarget, useContextMenu, onReveal, drag, home, useSessionAccess, t, }) {
    const { getTriggerProps } = useContextMenu();
    const row = node;
    const title = displayTitle(node, t);
    const selected = node.id === currentId;
    const statuses = sessionStatuses(node, t);
    const primaryStatus = statuses[0];
    const showStatus = primaryStatus.state !== 'idle';
    // Archived rows hold their in-place grayed slot, so manual reorder cannot
    // move them. Pinned rows drag within the pinned block: the browser gates
    // their drop targets to fellow pinned rows.
    const renamable = useSessionAccess(node.id, 'session/rename');
    const reorderable = useSessionAccess(node.id, 'workspace/insertSessionBefore');
    const draggable = drag !== undefined && reorderable && !row.blank && !row.archived;
    const menuOpen = useContextMenuTarget(target => target?.kind === 'session' && target.value.sessionId === node.id);
    const dragPreview = useRowDragPreview(css.dragPreview, DRAG_PREVIEW_PROPERTIES);
    const rowRef = useRef(null);
    const titleRef = useRef(null);
    // On macOS the click that activates the window is not delivered, but the
    // next click arrives with detail 2 and fires dblclick, so rename requires a
    // detail-1 press on this title since the window last lost focus.
    const renameArmed = useRef(false);
    const disarmRename = useRef(() => { renameArmed.current = false; }).current;
    const marquee = useTitleMarquee(titleRef);
    useEffect(() => {
        if (menuOpen || rowRef.current?.matches(':hover'))
            marquee.enter();
        else
            marquee.leave();
    }, [menuOpen, marquee]);
    useEffect(() => {
        if (onReveal === undefined)
            return;
        rowRef.current?.scrollIntoView({ block: 'nearest' });
        onReveal();
    }, [onReveal]);
    const ownRow = (_jsxs("div", { ref: rowRef, "data-row-key": `session:${node.id}`, className: clsx(css.sessionRow, selected && css.selected, menuOpen && css.menuOpen, row.archived && css.archived, drag?.marker === 'before' && css.dropBefore, drag?.marker === 'after' && css.dropAfter), role: "treeitem", "aria-selected": selected, "aria-description": row.archived ? t('toast.archivedNotOpenable') : undefined, tabIndex: 0, onClick: () => { onOpen(node.id); }, ...getTriggerProps(row.blank ? undefined : { kind: 'session', value: { sessionId: node.id, displayTitle: row.title } }, { onKeyDown: (event) => {
                if (event.target !== event.currentTarget || (event.key !== 'Enter' && event.key !== ' '))
                    return;
                if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey)
                    return;
                event.preventDefault();
                onOpen(node.id);
            } }), onPointerEnter: marquee.enter, onPointerLeave: () => { if (!menuOpen)
            marquee.leave(); }, draggable: draggable, onDragStart: !draggable
            ? undefined
            : (e) => {
                e.dataTransfer.effectAllowed = 'move';
                e.dataTransfer.setData('text/plain', node.id);
                drag.start();
                dragPreview.start(e);
            }, onDragEnd: () => { if (dragPreview.clear())
            drag?.end(); }, onDragOver: drag === undefined
            ? undefined
            : (e) => {
                if (!drag.active)
                    return;
                e.preventDefault();
                e.dataTransfer.dropEffect = 'move';
                drag.hover(rowHalf(e));
            }, onDrop: drag === undefined
            ? undefined
            : (e) => {
                if (!drag.active)
                    return;
                e.preventDefault();
                drag.drop(rowHalf(e));
            }, children: [_jsx("span", { className: css.slot, children: !row.archived && !row.blank && (showStatus
                    ? _jsx(SessionStatusDots, { statuses: statuses })
                    : renderSlot('sidebar.session.row.leading', { sessionId: node.id })) }), _jsx("span", { ref: titleRef, className: css.title, onMouseDown: (e) => {
                    if (e.detail !== 1)
                        return;
                    renameArmed.current = true;
                    // One shared function and options: repeated presses add one listener.
                    window.addEventListener('blur', disarmRename, { once: true });
                }, onDoubleClick: row.blank || !renamable
                    ? undefined
                    : (e) => {
                        e.stopPropagation();
                        if (!renameArmed.current)
                            return;
                        renameArmed.current = false;
                        onRenameRequest(node.id, row.title);
                    }, children: title }), !row.blank && (_jsxs("span", { className: css.rowResting, children: [_jsx("span", { className: clsx(css.time, primaryStatus.trailingLabel === undefined && css.timestamp), "aria-hidden": primaryStatus.trailingLabel === undefined ? undefined : true, children: primaryStatus.trailingLabel ?? timeLabel(row.updatedAt, now, t) }), renderSlot('sidebar.session.row.trailing', { sessionId: node.id }), row.pinned && !row.archived && _jsx(PinnedIndicator, { t: t })] })), !row.blank && (_jsxs("span", { className: css.rowActions, onClick: (e) => { e.stopPropagation(); }, children: [_jsx("button", { type: "button", className: css.iconButton, "aria-label": t('actions.session.aria', { name: title }), onPointerDown: (event) => { event.stopPropagation(); }, "data-context-menu-trigger": "", onClick: (event) => {
                            event.currentTarget.focus({ preventScroll: true });
                            const rect = event.currentTarget.getBoundingClientRect();
                            openSessionMenu(node.id, row.title, { x: rect.left, y: rect.bottom }, 'toggle');
                        }, children: _jsx(IconEllipsisOutlineRegular, {}) }), _jsx("span", { className: css.rowQuickActions, children: renderSlot('sidebar.workspaces.session.row.action', { sessionId: node.id, displayTitle: row.title }) })] }))] }));
    return (_jsx(HoverCard, { anchor: ownRow, content: _jsx(SessionHoverContent, { node: node, cwd: row.cwd === undefined ? undefined : abbreviateHomePath(row.cwd, home), now: now, renderSlot: renderSlot, t: t }), openDelayMs: 800, grouped: true, disabled: menuOpen || drag?.active === true, copyText: row.blank || row.title === '' ? undefined : row.title, copyLabel: t('copy'), copiedLabel: t('hover.copied') }));
}
//# sourceMappingURL=Rows.js.map