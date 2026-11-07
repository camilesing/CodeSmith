import type { RemoteResult } from '@codesmith/api-remotes/client';
import type { PropsLocale } from '@codesmith/client-ui-slots';
import type { ScheduleDeliveryHistoryRequest, ScheduleDeliveryHistoryResult } from '@codesmith/schedule/client';
import type { SessionId } from '@codesmith/session/types';
/** Read saved records without opening or activating the original Session. */
export interface DeliveryHistoryInjected {
    /**
     * Read a newest-first page of one task's saved deliveries.
     * @param request - Task, required page size, and optional older-page cursor.
     * @returns Remote success or failure; transport exceptions may reject the promise.
     */
    readonly loadHistory: (request: ScheduleDeliveryHistoryRequest) => Promise<RemoteResult<ScheduleDeliveryHistoryResult>>;
}
type Cursor = ScheduleDeliveryHistoryRequest['before'];
type Props = DeliveryHistoryInjected & PropsLocale<'schedule.manager'> & {
    id: ScheduleDeliveryHistoryRequest['id'];
    /** Open the Session that accepted one delivery. */
    onOpenSession: (id: SessionId) => void;
    /** Display titles of known Sessions, by id. */
    sessionTitles: Readonly<Record<string, string>>;
    /** Sessions that a delivery's open button may open. */
    availableSessionIds: readonly string[];
    latestMessageId: Cursor;
    /** IANA zone of the task's own wall-clock rule; undefined for a one-shot or interval task. */
    timeZone?: string | undefined;
};
/**
 * Render immutable saved deliveries; the parent keys this view by task.
 * Refreshes supersede pending older pages, and unmount ignores both late outcomes.
 * @param props - Task, latest receipt identity, Remote callback, Session links, task zone, and locale.
 * @returns Saved records and explicit loading, failure, and pagination actions.
 */
export declare function DeliveryHistory({ id, latestMessageId, timeZone, loadHistory, onOpenSession, sessionTitles, availableSessionIds, t, }: Props): import("react").JSX.Element;
export {};
//# sourceMappingURL=DeliveryHistory.d.ts.map