import type { Context } from '@codesmith/cordis';
import type { FileActions } from '../types.ts';
import { type FileActionKey } from './locales.ts';
export type { FileActions, FileActionSource, FileSaveResult } from '../types.ts';
declare module '@codesmith/cordis' {
    interface Context {
        fileActions: FileActions;
    }
}
declare module '@codesmith/client-ui-slots' {
    interface LocaleNamespaceMap {
        /** Original-file action labels and feedback. */
        fileActions: FileActionKey;
    }
}
/** The native adapter is optional; Web uses its own download and save capabilities. */
export declare const inject: never[];
/** @param ctx - plugin-owned Client context. */
export declare function apply(ctx: Context): void;
//# sourceMappingURL=index.d.ts.map