import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
/** One task's rule, saved deliveries, edits, runs, and linked Session. */
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import clsx from 'clsx';
import { Button, IconCloseOutlineRegular, IconEllipsisOutlineRegular, IconPauseOutlineRegular, IconPlayOutlineRegular, IconChevronRightOutlineRegular, Menu, MenuItemButton, Modal, Tooltip, } from '@codesmith/client-ui-primitives';
import { CatalogFeedback } from "./CatalogFeedback.js";
import { DeliveryHistory } from "./DeliveryHistory.js";
import { TaskEditorFields, draftTarget, newTaskDraft, projectChoices, reapplyTaskDraft, taskDraft, taskDraftIssue, taskDraftPatch } from "./TaskEditorFields.js";
import { timingFromDraft } from "./editor-timing.js";
import { formatAvailableAt, nextRunParts, recordTimeZone } from "./schedule-format.js";
import { sessionLinkState } from "./session-link.js";
import { taskNextRunAt, taskSessionId } from "./task-record.js";
import { NewSessionLimit } from "./NewSessionLimit.js";
import { RELATIVE_CLOCK_PERIOD_MS, useRelativeClock } from "./relative-clock.js";
import css from './TaskManagerPage.module.css';
/**
 * Own selected-task state shared by the Tasks page and task tabs.
 *
 * The shown task comes from the catalog, an unsaved form's CAS baseline, or a
 * confirmed deletion. The view and confirmation reset when selection changes;
 * the entry's store retains unsaved fields by task identity.
 * @param injected - detail actions, localized copy, and framework readers.
 * @param catalog - the owner's authoritative task catalog snapshot.
 * @param taskId - task the owner selected, or undefined when it selected none.
 * @returns the shown task and catalog row, its element id, its confirmation, and the detail props.
 */
export function useTaskDetail(injected, catalog, taskId) {
    const [retained, setRetained] = useState(null);
    const [tab, setTab] = useState('rule');
    const [confirmId, setConfirmId] = useState(null);
    const id = useId();
    const savedDraft = injected.useStore(state => taskId === undefined ? undefined : state.drafts[`task:${taskId}`]);
    useEffect(() => {
        setRetained(null);
        setTab('rule');
        setConfirmId(null);
    }, [taskId]);
    const record = taskId === undefined ? undefined : catalog.records.find(item => item.id === taskId);
    const edited = record === undefined && savedDraft?.base !== undefined
        && Object.keys(taskDraftPatch(savedDraft.draft, savedDraft.base)).length > 0
        ? savedDraft.base : undefined;
    const task = record ?? edited ?? (retained !== null && retained.id === taskId ? retained : undefined);
    return {
        record, task,
        props: {
            ...injected,
            status: catalog.status,
            deleting: catalog.deleting,
            id,
            onRetainTask: setRetained,
            tab,
            onTabChange: setTab,
            confirmId,
            onConfirm: setConfirmId,
        },
    };
}
/**
 * Render one new or existing task's editable rule or saved deliveries.
 * @param props - Task, catalog state, detail view, confirmation, locale, and actions.
 * @returns The detail region and its deletion confirmation dialog.
 */
export function TaskDetail(props) {
    return _jsx(TaskDetailView, { ...props }, props.task === undefined ? 'new' : `task:${props.task.id}`);
}
function TaskDetailView(props) {
    const { task, id, tab, onTabChange, onClose, onDelete, onConfirm, confirmId, onDeleted, onRetry, onOpenSession, loadHistory, status, deleting, useSessions, useWorkspaces, t, hostActions } = props;
    const sessions = useSessions(snapshot => snapshot);
    const workspaces = useWorkspaces(snapshot => snapshot);
    const key = task === undefined ? 'new' : `task:${task.id}`;
    const stored = props.useStore(state => state.drafts[key]);
    const [seed] = useState(newTaskDraft);
    const initial = stored?.initial ?? seed;
    const changes = useMemo(() => stored?.base === undefined ? undefined : taskDraftPatch(stored.draft, stored.base), [stored?.draft, stored?.base]);
    const storedDirty = changes !== undefined && Object.keys(changes).length > 0;
    const cached = stored?.base !== undefined && !storedDirty && !stored.pending && stored.failure === undefined
        && stored.base.updatedAt !== task?.updatedAt ? undefined : stored;
    const base = cached?.base ?? task;
    const input = cached?.draft ?? (task === undefined ? initial : taskDraft(task));
    const projects = projectChoices(workspaces.items, t('host.defaultProject'));
    // Until a directory is chosen, a new form follows the Workspace list as it arrives.
    const draft = task === undefined && input.cwd === '' ? { ...input, cwd: projects.defaultCwd } : input;
    const dirty = base === undefined
        ? JSON.stringify(draft) !== JSON.stringify({ ...initial, cwd: initial.cwd || projects.defaultCwd }) : storedDirty;
    const issue = dirty ? taskDraftIssue(draft, changes) : undefined;
    const valid = issue === undefined;
    const [actionPending, setPending] = useState(false);
    const [actionFailure, setFailure] = useState(null);
    const saving = cached?.pending === true;
    const pending = actionPending || saving;
    const failure = !pending && dirty && base !== undefined && task !== undefined && base.updatedAt !== task.updatedAt
        ? 'host.conflict' : actionFailure ?? cached?.failure ?? null;
    const [menuOpen, setMenuOpen] = useState(false);
    const moreRef = useRef(null);
    const panelRef = useRef(null);
    const scrollRef = useRef(null);
    const mounted = useRef(true);
    useLayoutEffect(() => {
        const scroll = scrollRef.current;
        const measure = () => {
            scroll.style.setProperty('--detail-scrollbar-space', `${scroll.offsetWidth - scroll.clientWidth}px`);
        };
        measure();
        const observer = new ResizeObserver(measure);
        observer.observe(scroll);
        return () => { observer.disconnect(); };
    }, [tab]);
    useEffect(() => {
        mounted.current = true;
        panelRef.current?.focus({ preventScroll: true });
        return () => { mounted.current = false; };
    }, []);
    const floorEndsAt = task?.deliveryFloorEndsAt === undefined ? undefined : Date.parse(task.deliveryFloorEndsAt);
    // A sample at the floor's end enables Run now when the interval passes, not up to one period later.
    const now = useRelativeClock(RELATIVE_CLOCK_PERIOD_MS, floorEndsAt);
    const deletingTask = task !== undefined && deleting.includes(task.id);
    const busy = pending || deletingTask || task !== undefined && status === 'loading';
    const executionActive = task?.executionActive === true;
    const deliveryFloor = floorEndsAt !== undefined && floorEndsAt > now;
    const linkedId = task === undefined ? undefined : taskSessionId(task);
    const linkedText = linkedId === undefined ? '' : sessions.byId[linkedId]?.displayTitle ?? linkedId;
    const linkedState = linkedId === undefined ? 'unavailable' : sessionLinkState(linkedId, sessions, workspaces);
    // The Host refuses to deliver to an archived target Session.
    const targetArchived = task?.target.kind !== 'new' && linkedState === 'archived';
    // Only the interval needs the row to explain itself: each other cause shows in the detail or is transient.
    const runHeld = busy || dirty || executionActive || targetArchived || !props.authoritative;
    const nextRunAt = task === undefined ? '' : taskNextRunAt(task);
    const next = task === undefined ? undefined : nextRunParts(nextRunAt, t('time.locale'), now, t);
    // SessionListState gives every listed id a byId row.
    const choices = sessions.ids.map(value => ({ value, label: sessions.byId[value].displayTitle }));
    const setDraft = (value) => {
        props.actions.setDraft(key, { base, draft: value, ...(task === undefined ? { initial } : {}),
            ...(cached?.failure === 'host.conflict' ? { failure: cached.failure } : {}) });
        setFailure(null);
    };
    const reset = () => { props.actions.clearDraft(key); setFailure(null); };
    const save = async () => {
        props.actions.setDraft(key, { base, draft, pending: true, ...(task === undefined ? { initial } : {}) });
        setFailure(null);
        const existingBase = base;
        const outcome = task === undefined
            ? await props.onCreateTask({ title: draft.title.trim(), prompt: draft.prompt.trim(),
                timing: timingFromDraft(draft, true), target: draftTarget(draft) })
            : await hostActions.edit({ id: task.id, action: 'update', patch: taskDraftPatch(draft, existingBase, true),
                expectedUpdatedAt: existingBase.updatedAt ?? null });
        if (outcome === 'saved' || 'created' in outcome) {
            props.actions.clearDraft(key);
            if (mounted.current && outcome !== 'saved')
                props.onCreated?.(outcome.created);
            if (mounted.current && outcome === 'saved')
                panelRef.current?.focus({ preventScroll: true });
        }
        else
            props.actions.setDraft(key, { base, draft, failure: outcome.failed, ...(task === undefined ? { initial } : {}) });
    };
    const setActive = async (task) => {
        setPending(true);
        const outcome = await hostActions.edit({ id: task.id, action: 'update', patch: { active: task.status !== 'active' }, expectedUpdatedAt: task.updatedAt ?? null });
        if (!mounted.current)
            return;
        setPending(false);
        setFailure(outcome === 'saved' ? null : outcome.failed);
    };
    const run = async (id) => {
        setMenuOpen(false);
        setPending(true);
        const outcome = await hostActions.run(id);
        if (!mounted.current)
            return;
        setPending(false);
        setFailure(outcome === 'queued' ? null : outcome.failed);
    };
    return _jsxs(_Fragment, { children: [_jsxs("aside", { ref: panelRef, tabIndex: -1, className: css.detail, id: id, "aria-busy": pending || deletingTask, "aria-label": t(task === undefined ? 'host.manualCreate' : 'detail.label'), onKeyDown: (event) => {
                    if (pending && event.key === 'Escape' && !event.defaultPrevented) {
                        event.preventDefault();
                        event.stopPropagation();
                    }
                }, children: [_jsxs("div", { className: css.detailTabsBar, children: [task === undefined ? _jsx("h2", { className: css.createTitle, children: t('host.manualCreate') })
                                : _jsx("div", { className: css.detailTabs, role: "tablist", "aria-label": t('detail.tabs'), children: ['rule', 'records'].map(value => _jsx("button", { type: "button", role: "tab", id: `${id}-${value}-tab`, "aria-controls": `${id}-${value}-panel`, tabIndex: tab === value ? 0 : -1, "aria-selected": tab === value, className: css.detailTab, onKeyDown: (event) => {
                                            if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey)
                                                return;
                                            if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key))
                                                return;
                                            event.preventDefault();
                                            const next = event.key === 'Home' ? 0 : event.key === 'End' ? 1 : value === 'rule' ? 1 : 0;
                                            onTabChange(next === 0 ? 'rule' : 'records');
                                            const button = event.currentTarget.parentElement?.children[next];
                                            if (button instanceof HTMLButtonElement)
                                                button.focus();
                                        }, onClick: () => { onTabChange(value); }, children: t(value === 'rule' ? 'detail.rule' : 'detail.records') }, value)) }), _jsxs("div", { className: css.detailActions, children: [task !== undefined && _jsxs(_Fragment, { children: [_jsx(Tooltip, { label: t(task.status === 'active' ? 'host.pause' : 'host.resume'), side: "bottom", delayMs: 500, portal: true, children: _jsx("span", { children: _jsx(Button, { size: "sm", className: css.detailIconButton, disabled: busy || dirty || executionActive || task.status === 'ended' || !props.authoritative, "aria-label": t(task.status === 'active' ? 'host.pause' : 'host.resume'), onClick: () => { void setActive(task); }, children: task.status === 'active' ? _jsx(IconPauseOutlineRegular, {}) : _jsx(IconPlayOutlineRegular, {}) }) }) }), _jsxs(Menu, { open: menuOpen, onClose: () => { setMenuOpen(false); }, align: "end", portal: true, autoFocus: true, anchor: _jsx(Button, { ref: moreRef, size: "sm", className: css.detailIconButton, "aria-label": t('detail.more'), "aria-haspopup": "menu", "aria-expanded": menuOpen, onClick: () => { setMenuOpen(value => !value); }, children: _jsx(IconEllipsisOutlineRegular, {}) }), children: [_jsx(MenuItemButton, { disabled: runHeld || deliveryFloor, disabledReason: deliveryFloor && !runHeld
                                                            ? t('host.runAfterInterval', { time: formatAvailableAt(floorEndsAt, t('time.locale'), now) }) : undefined, onSelect: () => { void run(task.id); }, children: t('host.run') }), _jsx(MenuItemButton, { disabled: busy || dirty || executionActive || !props.authoritative, onSelect: () => {
                                                            // The dialog returns focus to the control that held it on open, not to the removed menu row.
                                                            moreRef.current?.focus();
                                                            setMenuOpen(false);
                                                            onConfirm(task.id);
                                                        }, children: t('delete.action') })] })] }), onClose !== undefined && _jsx(Button, { size: "sm", className: css.detailIconButton, "aria-label": t(task === undefined ? 'host.closeCreate' : 'detail.close'), disabled: pending, onClick: onClose, children: _jsx(IconCloseOutlineRegular, {}) })] })] }), _jsxs("div", { ref: scrollRef, className: clsx(css.detailScroll, tab === 'records' && css.detailRecords), role: task === undefined ? undefined : 'tabpanel', id: `${id}-${tab}-panel`, "aria-labelledby": task === undefined ? undefined : `${id}-${tab}-tab`, tabIndex: 0, children: [_jsx(CatalogFeedback, { status: status, populated: true, onRetry: onRetry, t: t }), tab === 'rule' && _jsxs(_Fragment, { children: [task !== undefined && _jsxs(_Fragment, { children: [_jsx("header", { className: css.detailHeader, children: _jsx("h2", { className: css.readonlyName, children: task.title }) }), _jsx("div", { className: css.nextRun, children: _jsx("p", { children: task.status === 'active'
                                                        ? _jsxs(_Fragment, { children: [t('detail.nextRun'), " ", _jsx("time", { dateTime: nextRunAt, children: next?.absolute }), ' ', _jsx("span", { className: css.nextRunRelative, children: next?.relative })] })
                                                        : t(`status.${task.status}`) }) }), linkedId !== undefined && linkedState !== 'available' && task.target.kind === 'session'
                                                && _jsx("p", { className: css.detailNotice, role: "status", children: t(linkedState === 'archived' ? 'detail.sessionArchived'
                                                        : linkedState === 'loading' ? 'detail.sessionLoading' : 'detail.sessionUnavailable') }), executionActive && failure !== 'host.executionPending'
                                                && _jsx("p", { className: css.detailNotice, role: "status", children: t('host.executionActive') }), dirty && !props.authoritative && status === 'ready' && !deletingTask && failure !== 'detail.missing'
                                                && _jsx("p", { className: css.detailNotice, role: "status", children: t('detail.missing') })] }), failure === 'host.conflict' ? _jsxs("div", { className: clsx(css.notice, css.conflictNotice), children: [_jsx("p", { role: "alert", children: t(failure) }), changes !== undefined && dirty && task !== undefined && props.authoritative && status === 'ready'
                                                && _jsx(Button, { variant: "outline", size: "sm", disabled: busy, onClick: () => {
                                                        props.actions.setDraft(key, { base: task, draft: reapplyTaskDraft(draft, task, changes) });
                                                        setFailure(null);
                                                        panelRef.current?.focus({ preventScroll: true });
                                                    }, children: t('host.reapply') })] }) : failure === 'host.executionPending' ? _jsx("p", { className: css.detailNotice, role: "status", children: t(failure) })
                                        : failure !== null && _jsx("p", { className: css.saveFailure, role: "alert", children: t(failure) }), _jsx(TaskEditorFields, { draft: draft, onChange: setDraft, disabled: pending || executionActive || deletingTask, projects: projects, original: base, t: t }), draft.target === 'new'
                                        && _jsx(NewSessionLimit, { load: hostActions.loadNewSessionInterval, t: t }), failure === null && issue !== undefined && issue !== 'incomplete'
                                        && _jsx("p", { className: css.saveFailure, role: "status", children: t(issue) })] }), tab === 'records' && task !== undefined && _jsx(DeliveryHistory, { id: task.id, latestMessageId: task.lastDelivery?.messageId, timeZone: recordTimeZone(task), loadHistory: loadHistory, onOpenSession: onOpenSession, sessionTitles: Object.fromEntries(choices.map(choice => [choice.value, choice.label])), availableSessionIds: sessions.ids.filter(value => sessionLinkState(value, sessions, workspaces) === 'available'), t: t })] }), task !== undefined && (task.target.kind !== 'new' || linkedId !== undefined) && _jsxs("footer", { className: css.hostSessionFooter, children: [_jsx("span", { children: t(task.target.kind === 'new' ? 'host.latestConversation' : 'detail.session') }), linkedId === undefined || (task.target.kind === 'new_once' && linkedState === 'unavailable')
                                ? _jsx("span", { className: css.hostSessionEmpty, children: t('host.sessionAfterFirstRun') })
                                : _jsxs(Button, { variant: "outline", disabled: linkedState !== 'available', "aria-label": linkedText === linkedId ? t('detail.openSession') : t('detail.openSessionTitle', { title: linkedText }), onClick: () => { onOpenSession(linkedId); }, children: [linkedText, _jsx(IconChevronRightOutlineRegular, {})] })] }), (tab === 'rule' || dirty) && _jsxs("footer", { className: css.saveFooter, children: [_jsx("span", { className: css.saveNotice, children: dirty ? t('rule.unsaved') : '' }), _jsx(Button, { disabled: pending || task !== undefined && !dirty && failure === null, onClick: () => { reset(); if (task === undefined)
                                    onClose?.(); }, children: t('rule.cancel') }), _jsx(Button, { variant: "primary", disabled: busy || !dirty || !valid || failure === 'host.conflict'
                                    || task !== undefined && (!props.authoritative || executionActive), onClick: () => { void save(); }, children: t(saving ? task === undefined ? 'host.creating' : 'rule.saving' : task === undefined ? 'host.create' : 'rule.save') })] })] }), task !== undefined && _jsx(Modal, { open: confirmId === task.id, className: css.confirmDialog, contentClassName: css.confirmContent, title: t('delete.title'), description: t('delete.description'), closeLabel: t('delete.close'), onClose: () => { onConfirm(null); }, footer: _jsxs("div", { className: css.confirmActions, children: [_jsx(Button, { "data-modal-autofocus": true, onClick: () => { onConfirm(null); }, children: t('delete.cancel') }), _jsx(Button, { disabled: busy, onClick: () => {
                                onConfirm(null);
                                setPending(true);
                                // The refresh that drops the row lands before the outcome, so the owner retains the task until then.
                                props.onRetainTask(task);
                                void onDelete(task.id).then((outcome) => {
                                    if (!mounted.current)
                                        return;
                                    setPending(false);
                                    if (outcome === 'deleted') {
                                        props.actions.clearDraft(key);
                                        onDeleted();
                                    }
                                    else
                                        props.onRetainTask(null);
                                });
                            }, children: t('delete.confirm') })] }), children: _jsx("p", { className: css.confirmPrompt, children: task.prompt }) })] });
}
//# sourceMappingURL=TaskDetail.js.map