import { resolveWorkspacePath } from '@deepseek-ai/dsh-util-workspace-path';
/**
 * Bind path resolution to the originating Session without capturing its current directory.
 * @param ctx - Chat plugin context.
 * @param sessionId - Original Session identity.
 * @returns Resolver using the live working directory, falling back to the workspace root.
 */
export function sessionFilePath(ctx, sessionId) {
    return (path) => {
        const cwd = ctx.sessions.list.getSnapshot().byId[sessionId]?.cwd;
        const current = ctx.sessions.binding(sessionId)?.session.projections.faceOf('workingDirectory').getSnapshot();
        return resolveWorkspacePath(current ?? cwd, path);
    };
}
//# sourceMappingURL=file-path.js.map