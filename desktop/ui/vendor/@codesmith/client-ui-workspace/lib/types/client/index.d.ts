import type { Context } from '@codesmith/cordis';
import type { WorkspaceId, WorkspaceSnapshot } from '@codesmith/api-workspace-controller/client';
import type { RemoteEndpoint } from '@codesmith/api-remote-access/client';
import type { SnapshotSelectorHook } from '@codesmith/client-ui-slots';
import { type WorkspaceKey } from './locales.ts';
export type { StartSessionOptions, UiWorkspace } from './navigation.ts';
export type { DirectoryFlowOwnerProps, DirectoryFlowSlotName, DirectoryPickingHooks, DirectoryPickingInjected, MenuOpenState, RowToast, SessionRenameTarget, SessionRowOwnerProps, UseMenuOpenState, WorkspaceBrowserInjected, SessionRowScheduleOwnerProps, WorkspaceBrowserProps, WorkspacePickerInjected, WorkspacePickerProps, } from './contract/slots.ts';
export type { WorkspaceKey } from './locales.ts';
declare module '@codesmith/client-ui-slots' {
    interface GlobalStandardProps {
        /** Selector hook over the pure Workspace Controller snapshot. */
        useWorkspaces: SnapshotSelectorHook<WorkspaceSnapshot>;
        /** Whether the current Peer may call `action` on one Workspace resource. */
        useWorkspaceAccess: (workspaceId: WorkspaceId, action: RemoteEndpoint) => boolean;
    }
    interface LocaleNamespaceMap {
        /** The workspace browsing region and pick/create flow copy. */
        workspace: WorkspaceKey;
    }
}
declare module '@codesmith/api-session-controller/client' {
    interface SessionReferenceSourceMap {
        workspaceOperation: unknown;
    }
}
/**
 * Required services (cordis fiber inject). The target slots are declared by
 * the ui-sidebar / ui-conversation applies, whose activation order relative
 * to this one is NOT constrained: cds.client.inject edges are informational
 * (loading/prefetch metadata, never apply sequencing) and neither owner
 * provides a waitable service. apply therefore depends on each slot
 * declaration through `slots.inject()` instead of assuming order.
 */
export declare const inject: string[];
/**
 * Register the browser and picker once their slot declarations are on the
 * ledger. Inject factories return plain callbacks; data reads use the
 * framework's global hooks.
 * @param ctx - client root context.
 */
export declare function apply(ctx: Context): void;
//# sourceMappingURL=index.d.ts.map