import { Service } from '@codesmith/cordis';
import type { Context } from '@codesmith/cordis';
import type { SessionId } from '@codesmith/session/types';
import { ModelDirectory } from './directory.ts';
declare module '@codesmith/cordis' {
    interface Context {
        modelDirectories: ModelDirectoryResolver;
    }
}
/** The `ctx.modelDirectories` session model-selection service. */
export declare class ModelDirectoryResolver extends Service {
    static inject: string[];
    private readonly live;
    private readonly catalog;
    /**
     * @param ctx - owning root context (the service registers itself as `models`).
     */
    constructor(ctx: Context);
    /**
     * Resolve the per-session shared directory (lazy; the scope disposer
     * removes and disposes it). Unknown sessions fail loud.
     * @param sessionId - the owning session.
     * @returns the resident directory both entries share.
     */
    directoryFor(sessionId: SessionId): ModelDirectory;
}
//# sourceMappingURL=service.d.ts.map