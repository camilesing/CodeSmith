import type { Context as ClientContext } from '@codesmith/cordis';
export type {} from './user-text-rules.ts';
declare module '@codesmith/api-session-controller/client' {
    interface SessionReferenceSourceMap {
        /** File and Session candidates waiting for initial history and their RPC results. */
        referenceCandidates: unknown;
    }
}
/** Required services: the trigger registry, the Remote namespaces, and the copy. */
export declare const inject: string[];
/**
 * Register the combined `@file` / `@session` source and independent file/folder text providers.
 * @param ctx - client root context.
 */
export declare function apply(ctx: ClientContext): void;
//# sourceMappingURL=index.d.ts.map