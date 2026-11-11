import { fileMediaUrl } from '@codesmith/util-workspace-path';
import mime from 'mime';
/**
 * Register original-byte operations without requiring an application-opening plugin.
 * @param ctx - menu and authorized file-read services.
 * @param actions - file-action owner.
 * @param t - localized labels and feedback.
 */
export function registerFileMenus(ctx, actions, t) {
    const lifetime = new AbortController();
    ctx.effect(() => () => { lifetime.abort(); }, 'file actions: feedback lifetime');
    const read = (sessionId, path, mediaType, error) => async (signal) => {
        const result = await ctx.remote.workspaceFiles.readBytes(sessionId, path, {}, signal);
        if (!result.ok)
            throw new Error(error);
        return new Blob([new Uint8Array(result.value.data)], { type: mediaType });
    };
    const save = async (source, mode) => {
        const result = await actions.save(source, mode);
        lifetime.signal.throwIfAborted();
        if (result.status === 'cancelled')
            return;
        ctx.contextMenus.notify(t(result.status === 'requested' ? 'menu.downloadStarted' : mode === 'download' ? 'menu.downloaded' : 'menu.saved'), true, result.status !== 'saved' ? undefined : {
            label: t(document.documentElement.dataset.platform === 'darwin' ? 'menu.finder' : 'menu.reveal'),
            failureLabel: t('path.revealError'),
            run: async () => {
                lifetime.signal.throwIfAborted();
                const opened = await ctx.remote.session.openWorkspacePath({ path: result.path, action: 'reveal' }, lifetime.signal);
                if (!opened.ok)
                    throw new Error('File location unavailable');
            },
        });
    };
    const copy = async (source) => {
        await actions.copyImage(source);
        ctx.contextMenus.notify(t('menu.copied'), true);
    };
    ctx.effect(() => ctx.contextMenus.register({
        id: 'file-actions.file', target: 'file', order: 200,
        resolve: async ({ sessionId, path }, signal) => {
            let absolutePath;
            try {
                const result = await ctx.remote.workspaceFiles.stat(sessionId, path, signal);
                absolutePath = result.ok ? result.value.absolutePath : undefined;
            }
            catch (_error) {
                // Failed metadata disables operations that require the source file.
                signal.throwIfAborted();
            }
            signal.throwIfAborted();
            const available = absolutePath !== undefined;
            const mediaType = mime.getType(path) ?? '';
            const source = {
                url: fileMediaUrl(document.baseURI, absolutePath ?? path) ?? path,
                name: path.slice(Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\')) + 1),
                mediaType,
                read: read(sessionId, path, mediaType, 'Original file unavailable'),
            };
            return [
                ...mediaType.startsWith('image/') ? [
                    { id: 'copyImage', icon: { key: 'file-actions.copy' }, separatorBefore: true, failureLabel: t('menu.copyFailed'),
                        label: t('menu.copyImage'), disabled: !available, run: () => copy(source) },
                ] : [],
                ...mediaType.startsWith('image/') || !actions.canSaveAs ? [
                    { id: 'download', icon: { key: 'file-actions.download' }, failureLabel: t('menu.saveFailed'),
                        label: t('menu.download'), disabled: !available, run: () => save(source, 'download') },
                ] : [],
                ...actions.canSaveAs ? [
                    { id: 'saveAs', icon: { key: 'file-actions.save' }, failureLabel: t('menu.saveFailed'), label: t('menu.saveAs'),
                        disabled: !available, run: () => save(source, 'saveAs') },
                ] : [],
            ];
        },
    }), 'file actions: file menu');
    ctx.effect(() => ctx.contextMenus.register({
        id: 'file-actions.image', target: 'image', order: 100,
        resolve: (target) => {
            const { sessionId, path } = target;
            const name = target.name ?? path?.split(/[\\/]/u).pop();
            const mediaType = target.mediaType || mime.getType(name ?? target.url) || undefined;
            const source = {
                ...target,
                ...(name === undefined ? {} : { name }),
                ...(mediaType === undefined ? {} : { mediaType }),
                ...(path === undefined || sessionId === undefined ? {} : {
                    read: read(sessionId, path, mediaType ?? '', 'Original image unavailable'),
                }),
            };
            return [
                { id: 'copy', icon: { key: 'file-actions.copy' }, failureLabel: t('menu.copyFailed'), label: t('menu.copyImage'), run: () => copy(source) },
                { id: 'download', icon: { key: 'file-actions.download' }, failureLabel: t('menu.saveFailed'), label: t('menu.download'), run: () => save(source, 'download') },
                ...actions.canSaveAs ? [
                    { id: 'saveAs', icon: { key: 'file-actions.save' }, failureLabel: t('menu.saveFailed'), label: t('menu.saveAs'),
                        run: () => save(source, 'saveAs') },
                ] : [],
            ];
        },
    }), 'file actions: image menu');
}
//# sourceMappingURL=context-menus.js.map