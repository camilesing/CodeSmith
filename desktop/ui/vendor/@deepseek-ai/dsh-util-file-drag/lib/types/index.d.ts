import type { Branded } from '@deepseek-ai/dsh-brand';
/** Browser transfer token type; original bytes stay with the application owner. */
export declare const FILE_DRAG_TOKEN_TYPE = "application/x-dsh-file";
/** Opaque preparation identity, minted and resolved only by the desktop main process. */
export type FileDragHandle = Branded<'FileDragHandle'>;
/** Caller-minted identity for cancelling a native preparation before it returns a handle. */
export type FileDragRequestId = Branded<'FileDragRequestId'>;
/** Caller-supplied export naming, shared by Web and Desktop. */
export interface DragFileName {
    readonly name?: string | undefined;
    readonly mediaType: string;
    readonly outputExtension?: string | undefined;
}
/** Native preparation copies bytes or asks the Host to authorize borrowing a resource. */
export type DesktopFileSource = DragFileName & ({
    readonly kind: 'bytes';
    readonly bytes: Uint8Array;
} | {
    readonly kind: 'resource';
    readonly address: string;
    readonly sha256?: string;
    readonly size?: number;
});
/** Native file operations; no request accepts a raw renderer filesystem path. */
export interface DesktopFileDrag {
    /**
     * @param source - bytes or a verifiable resource.
     * @param requestId - cancellation identity.
     * @returns a handle, or undefined if the resource needs a byte copy.
     */
    prepare(this: void, source: DesktopFileSource, requestId: FileDragRequestId): Promise<FileDragHandle | undefined>;
    /** Cancel unfinished preparation and release its unused resources. @param requestId - preparation identity. */
    cancel(this: void, requestId: FileDragRequestId): void;
    /** @param handle - prepared file. @returns whether the OS accepted the drag. */
    start(this: void, handle: FileDragHandle): Promise<boolean>;
    /** Release unused preparation; OS-delivered temporary files receive a bounded read grace period. @param handle - prepared file. */
    release(this: void, handle: FileDragHandle): void;
}
/**
 * Resolve one platform-safe export filename. Explicit suffix wins; compatible suffixes survive.
 * A known conflicting suffix is replaced from MIME; unknown MIME never rejects a supplied name.
 * @param request - caller name, declared content type, and optional explicit suffix.
 * @returns cleaned filename, with MIME-derived suffix only when needed.
 */
export declare function resolveDragFileName(request: DragFileName): string;
//# sourceMappingURL=index.d.ts.map