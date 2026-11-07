import { resolveWorkspacePath } from '@codesmith/util-workspace-path';
/**
 * Bind a toolview's path reader to its originating Session.
 * @param ctx - Tool plugin context.
 * @returns Existing Slot registration's inject callback.
 */
export function toolFileInject(ctx) {
    return sessionId => ({
        resolveFilePath: (path) => {
            const cwd = ctx.sessions.list.getSnapshot().byId[sessionId]?.cwd;
            const current = ctx.sessions.binding(sessionId)?.session.projections.faceOf('workingDirectory').getSnapshot();
            return resolveWorkspacePath(current ?? cwd, path);
        },
    });
}
//# sourceMappingURL=file-menu.js.map