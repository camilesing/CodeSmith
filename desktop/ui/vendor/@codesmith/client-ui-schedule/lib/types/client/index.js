/**
 * Browser catalogs for retained Host tasks and the selected Session's active
 * reminders, plus the right-Sidebar page that shows one task's detail.
 *
 * The page type reaches the Sidebar through its public path only: the
 * definition into `ctx.sidebarRightTabs`, the body into the keyed
 * `sidebar.right.pane.tab` seat, and the chip title into
 * `sidebar.right.pane.tab.title`, both under the definition's `id`. Because the
 * Sidebar persists a tab's layout record and not the parameters its opener
 * passed, the page also owns a binding from each tab page to the task it last
 * showed, which its body and chip read back after a reload. The Session
 * header entry and the task tab share one Host catalog source, so opening or
 * deleting from either view reads and refreshes the same records.
 *
 * The created task of a `schedule_create` call renders at Turn level: a Turn
 * Definition publishes the settled result against its Turn, and ui-chat's
 * `conversation.chat.turnTail` list seat renders the card beneath
 * the closing prose, outside the collapsible Tool group, opening the same
 * right-Sidebar detail the header entry opens. The call's Tool-group cell
 * stays with ui-tool's generic keyed tool view, so this package registers no
 * `tool.call.toolview` entry for the wire name.
 *
 * Two ambient surfaces read the ONE Host catalog the page owns: ui-workspace's
 * `sidebar.session.row.leading` seat marks an idle row whose Session has an
 * active task, and its `sidebar.session.row.hover` seat lists those tasks
 * inside the row's hover card. Both project the same source, so N visible rows
 * issue one query rather than one per row. The header entry projects the same
 * source onto the open Session's active tasks.
 */
import { createCatalogSource } from "./catalog-source.js";
import { createClearEnded } from "./clear-ended.js";
import { createDeleteToastSource, ScheduleDeleteToast } from "./DeleteToast.js";
import { SCHEDULE_TASK_ID, SCHEDULE_TASK_KIND, scheduleTaskDefinition } from "./definition.js";
import { ScheduleCatalogAction } from "./ScheduleCatalogAction.js";
import { ScheduleTurnCard } from "./ScheduleTurnCard.js";
import { scheduleTurnDefinition } from "./schedule-turn.js";
import { ScheduleTaskTab } from "./ScheduleTaskTab.js";
import { ScheduleTaskTabTitle } from "./ScheduleTaskTabTitle.js";
import { SessionScheduleHover } from "./SessionScheduleHover.js";
import { SessionScheduleMark } from "./SessionScheduleMark.js";
import { TaskTabBindings } from "./task-tab-bindings.js";
import { createEditorStore } from "./editor-store.js";
import { TaskManagerPage } from "./TaskManagerPage.js";
import { TaskManagerIcon } from "./TaskManagerIcon.js";
import { sessionLinkState } from "./session-link.js";
import { en, ja, es, fr, de, ru, pt, NS, zh } from "./locales.js";
import { en as managerEn, ja as managerJa, es as managerEs, fr as managerFr, de as managerDe, ru as managerRu, pt as managerPt, zh as managerZh } from "./task-manager-locales.js";
const MANAGER_NS = 'schedule.manager';
const PANEL_ID = 'schedules';
/** Required services for catalogs, ambient Session marks, the right Sidebar, Remote queries, linked-Session navigation, and access. */
export const inject = [
    'slots', 'locale', 'remote', 'remote.schedule', 'conversation', 'uiConversation', 'uiWorkspace', 'sessions',
    'workspaces', 'sidebarRightTabs', 'sidebarRight', 'remoteAccess',
];
async function settle(call) {
    try {
        return await call();
    }
    catch (_error) {
        // A rejected call leaves the outcome unknown, so its caller shows the same localized retry as a Remote failure.
        return undefined;
    }
}
function inputCorrection(error) {
    if (error.code !== 'schedule/input-invalid')
        return undefined;
    const { code } = error.details;
    if (code === 'not_future')
        return 'host.notFuture';
    return code === 'invalid_rule' || code === 'frequency_too_high' || code === 'invalid_time_zone' || code === 'time_out_of_range'
        ? 'host.invalidRule' : undefined;
}
/**
 * Register the Host task page, the Session-header reminder catalog, the
 * right-Sidebar page that shows one task's detail, and the transcript card of
 * one created task while the current Peer may read the task catalog.
 * @param ctx - browser services used by these contributions.
 */
export function apply(ctx) {
    ctx.effect(() => ctx.locale.register(NS, { zh, en, ja, es, fr, de, ru, pt }), 'ui-schedule: dictionaries');
    const managerDictionaries = {
        zh: managerZh,
        en: managerEn,
        ja: managerJa,
        es: managerEs,
        fr: managerFr,
        de: managerDe,
        ru: managerRu,
        pt: managerPt,
    };
    ctx.effect(() => ctx.locale.register(MANAGER_NS, managerDictionaries), 'ui-schedule: manager dictionaries');
    const t = ctx.locale.bind(MANAGER_NS);
    ctx.effect(() => ctx.sidebarRightTabs.register(scheduleTaskDefinition(t)), 'ui-schedule: task tab type');
    // The created task is a Turn-level element, not a Tool-group row: the Turn
    // Definition publishes the settled result and the tail list entry below
    // renders the card beneath the closing prose.
    ctx.uiConversation.events.register(scheduleTurnDefinition);
    ctx.remoteAccess.whenAccess('schedule/catalog', function* () {
        const manager = createCatalogSource({
            list: () => ctx.remote.schedule.catalog(),
            remove: async (id) => {
                const result = await ctx.remote.schedule.edit({ id, action: 'delete' });
                if (!result.ok)
                    return result;
                return { ok: true, value: 'deleted' in result.value ? { id, deleted: true } : { id, deleted: false, code: 'schedule_not_found' } };
            },
            subscribeChanged: listener => ctx.remote.$on('schedule/changed', listener),
            subscribeReset: listener => ctx.on('connection/reset', listener),
        });
        // One outcome store behind one `shell.overlay` entry announces every deletion
        // this plugin's surfaces settle, so the notice outlives the panel, popover, or
        // tab the deletion started in.
        const deleteToast = createDeleteToastSource();
        const reportedDelete = (onDelete) => async (id) => {
            const outcome = await onDelete(id);
            deleteToast.report(outcome);
            return outcome;
        };
        yield ctx.slots.inject('shell.overlay', () => ctx.slots.register({
            name: 'shell.overlay', id: 'schedule.delete-toast', locale: MANAGER_NS,
            inject: () => ({ hooks: deleteToast.hooks, dismiss: deleteToast.dismiss }),
        }, ScheduleDeleteToast));
        // The Host answered, so this readback must not join a read that was sent
        // before it: naming every request seen so far makes the source issue a new
        // one, the way the post-deletion readback supersedes.
        const refresh = () => manager.onRetry(manager.hooks.catalog.getSnapshot().readRequest);
        const openSession = (id) => {
            if (sessionLinkState(id, ctx.sessions.list.getSnapshot(), ctx.workspaces.list.getSnapshot()) === 'available') {
                ctx.uiWorkspace.openSession(id);
            }
        };
        /**
         * Remove every ended Host task and read the catalog back.
         *
         * The readback names every request seen so far, so it supersedes a read sent
         * before the removal instead of joining it.
         * @returns the settled bulk-removal outcome.
         */
        const clearEnded = createClearEnded({
            removeInactive: () => ctx.remote.schedule.deleteInactive(),
            refresh,
        });
        const detail = {
            hooks: manager.hooks,
            onDelete: reportedDelete(manager.onDelete),
            onRetry: manager.onRetry,
            loadHistory: request => ctx.remote.schedule.history(request),
            onOpenSession: openSession,
            hostActions: {
                edit: async (request) => {
                    const result = await settle(() => ctx.remote.schedule.edit(request));
                    if (result === undefined)
                        return { failed: 'host.saveError' };
                    if (!result.ok)
                        return { failed: inputCorrection(result.error) ?? 'host.saveError' };
                    await refresh();
                    if (!('code' in result.value))
                        return 'saved';
                    return { failed: result.value.code === 'schedule_conflict' ? 'host.conflict' : 'detail.missing' };
                },
                run: async (id) => {
                    const result = await settle(() => ctx.remote.schedule.run({ id }));
                    if (result === undefined || !result.ok)
                        return { failed: 'host.runError' };
                    await refresh();
                    if (!('code' in result.value))
                        return 'queued';
                    return { failed: result.value.code === 'schedule_execution_pending' ? 'host.executionPending' : 'detail.missing' };
                },
                loadNewSessionInterval: async () => {
                    const result = await settle(() => ctx.remote.schedule.deliveryLimits());
                    return result?.ok === true ? result.value.newSessionIntervalSeconds : undefined;
                },
            },
        };
        // The Sidebar persists a tab's layout record and not its navigation
        // parameters, so this page type keeps the task it last showed per Session and
        // tab id. That Session's committed tab ids bound every write, so a closed tab
        // leaves no entry behind.
        const taskBindings = new TaskTabBindings(sessionId => ctx.sidebarRight.tabsIn(sessionId).map(tab => tab.id));
        yield ctx.slots.inject('sidebar.right.pane.tab', () => ctx.slots.register({
            name: 'sidebar.right.pane.tab',
            key: SCHEDULE_TASK_ID,
            locale: MANAGER_NS,
            store: createEditorStore(),
            inject: () => ({ ...detail, taskBindings }),
        }, ScheduleTaskTab));
        yield ctx.slots.inject('sidebar.right.pane.tab.title', () => ctx.slots.register({
            name: 'sidebar.right.pane.tab.title',
            key: SCHEDULE_TASK_ID,
            locale: MANAGER_NS,
            inject: () => ({ hooks: detail.hooks, taskBindings }),
        }, ScheduleTaskTabTitle));
        yield ctx.slots.inject('main', () => ctx.slots.register({
            name: 'main',
            key: PANEL_ID,
            locale: MANAGER_NS,
            store: createEditorStore(),
            inject: () => ({
                ...detail,
                onNewTask: () => { ctx.uiWorkspace.startSession(undefined, { prompt: t('host.createPrompt'), whenDraftExists: 'append' }); },
                onClearEnded: async () => {
                    const outcome = await clearEnded();
                    deleteToast.reportClear(outcome);
                },
                onCreateTask: async (request) => {
                    const result = await settle(() => ctx.remote.schedule.create(request));
                    if (result === undefined)
                        return { failed: 'host.createError' };
                    if (!result.ok)
                        return { failed: inputCorrection(result.error) ?? 'host.createError' };
                    manager.retainCreated(result.value);
                    await refresh();
                    return { created: result.value };
                },
            }),
        }, TaskManagerPage));
        yield ctx.slots.inject('sidebar.panellist', () => ctx.slots.register({
            name: 'sidebar.panellist',
            id: PANEL_ID,
            order: 10,
            locale: MANAGER_NS,
            label: () => t('panel'),
        }, TaskManagerIcon));
        yield ctx.slots.inject('conversation.chat.turnTail', () => ctx.slots.register({
            name: 'conversation.chat.turnTail',
            id: 'schedule-created',
            order: 20,
            locale: MANAGER_NS,
            inject: (sessionId) => ({
                hooks: { catalog: manager.hooks.catalog },
                onRetry: manager.onRetry,
                openTaskDetail: (id) => {
                    ctx.sidebarRight.openTab(SCHEDULE_TASK_KIND, { params: { sessionId, id } });
                },
            }),
        }, ScheduleTurnCard));
        yield ctx.slots.inject('conversation.session.header.utilities', () => ctx.slots.register({
            name: 'conversation.session.header.utilities',
            id: 'schedule-catalog',
            // Sits left of the overflow menu (order 0) and right of the header's
            // leading utilities (order -10), matching the shipped mock.
            order: -5,
            locale: NS,
            inject: sessionId => ({
                hooks: manager.hooks,
                onRetry: manager.onRetry,
                onDelete: detail.onDelete,
                openTaskDetail: (id) => {
                    ctx.sidebarRight.openTab(SCHEDULE_TASK_KIND, { params: { sessionId, id } });
                },
            }),
        }, ScheduleCatalogAction));
        yield ctx.slots.inject('sidebar.session.row.leading', () => ctx.slots.register({
            name: 'sidebar.session.row.leading',
            id: 'schedule-mark',
            order: 10,
            locale: NS,
            inject: () => ({ hooks: { catalog: manager.hooks.catalog } }),
        }, SessionScheduleMark));
        yield ctx.slots.inject('sidebar.session.row.hover', () => ctx.slots.register({
            name: 'sidebar.session.row.hover',
            id: 'schedule-tasks',
            order: 10,
            locale: NS,
            inject: () => ({ hooks: { catalog: manager.hooks.catalog } }),
        }, SessionScheduleHover));
    });
}
//# sourceMappingURL=index.js.map