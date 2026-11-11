/** Original-file actions independent of menu or image component ownership. */
import { IconCopyOutlineRegular, IconDownloadOutlineRegular, IconSaveOutlineRegular } from '@codesmith/client-ui-primitives';
import { registerFileMenus } from "./context-menus.js";
import { zh, en, ja, es, fr, de, ru, pt } from "./locales.js";
import { BrowserFileActions } from "./files.js";
/** The native adapter is optional; Web uses its own download and save capabilities. */
export const inject = [];
/** @param ctx - plugin-owned Client context. */
export function apply(ctx) {
    const desktop = window.cdsDesktop?.fileActions;
    const actions = new BrowserFileActions(desktop);
    ctx.provide('fileActions', actions);
    ctx.effect(() => () => { actions.dispose(); }, 'file actions: lifetime');
    ctx.inject(['contextMenus', 'slots', 'locale', 'remote', 'remote.workspaceFiles', 'remote.session'], (child) => {
        child.effect(() => child.locale.register('fileActions', { zh, en, ja, es, fr, de, ru, pt }), 'file actions: locale');
        child.slots.inject('contextMenu.icon', function* () {
            for (const [key, Icon] of Object.entries({
                'file-actions.copy': IconCopyOutlineRegular, 'file-actions.download': IconDownloadOutlineRegular,
                'file-actions.save': IconSaveOutlineRegular,
            }))
                yield child.slots.register({ name: 'contextMenu.icon', key }, Icon);
        });
        registerFileMenus(child, actions, child.locale.bind('fileActions'));
    });
}
//# sourceMappingURL=index.js.map