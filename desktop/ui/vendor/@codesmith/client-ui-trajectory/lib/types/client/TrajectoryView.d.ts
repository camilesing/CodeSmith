/** Trajectory view: compact summary over a turn-aware event ledger. */
import type { ConvViewProps, MessageImageLoader } from '@codesmith/client-ui-conversation/client';
import type { InjectFace, PropsLocale, PropsRenderSlots } from '@codesmith/client-ui-slots';
import type { SnapshotStore } from '@codesmith/client-store';
import type { JsonTreeProps } from '@codesmith/client-ui-primitives';
import type { WatchToolResult } from './watch-tool-result.ts';
/** Session-bound controls not already supplied by the conversation view slot. */
export interface TrajectoryViewInjected {
    /** Shared wrapping preference read by expansion handlers. */
    jsonStringWrapping?: Omit<NonNullable<JsonTreeProps['stringWrapping']>, 'label'>;
    hooks: {
        duration: SnapshotStore<boolean>;
    };
    loadOlder: () => Promise<boolean>;
    loadImage: MessageImageLoader;
    setActualDuration: (actualDuration: boolean) => void;
    watchToolResult?: WatchToolResult;
}
export declare function TrajectoryView({ useSession, useTrajectory, useDuration, watchToolResult, loadOlder, loadImage, setActualDuration, viewRequest, completeViewRequest, renderSlot, t, jsonStringWrapping, }: ConvViewProps & Pick<PropsRenderSlots<'conversation.trajectory.images'>, 'renderSlot'> & InjectFace<TrajectoryViewInjected> & PropsLocale<'trajectory'>): import("react").JSX.Element;
//# sourceMappingURL=TrajectoryView.d.ts.map