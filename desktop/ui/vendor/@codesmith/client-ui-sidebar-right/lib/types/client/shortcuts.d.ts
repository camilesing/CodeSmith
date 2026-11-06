/** Sidebar-owned commands resolved against the currently mounted page. */
import type { Shortcuts } from '@codesmith/client-shortcuts/client';
import type { TranslateNS } from '@codesmith/client-locale/client';
import type { SidebarRightController } from './service.ts';
/**
 * Register the sidebar commands over the controller used by its visible controls.
 * @param shortcuts - effective-binding registry for this window.
 * @param sidebar - current Session and page owner.
 * @param t - current localized command and unavailable labels.
 * @param closeWindow - private native close operation using the current configuration revision.
 * @returns release callback for the commands.
 */
export declare function registerSidebarShortcuts(shortcuts: Pick<Shortcuts, 'register' | 'runtime'>, sidebar: SidebarRightController, t: TranslateNS<'sidebarRight'>, closeWindow: () => void): () => void;
//# sourceMappingURL=shortcuts.d.ts.map