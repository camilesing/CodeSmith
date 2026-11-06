/**
 * Production client composition without the page: mount the Loader over a
 * module system, create every manifest row, wait for quiescence, and audit
 * activation. `AppWebEntry` and the whole-client test carrier both call it.
 * @module @deepseek-ai/dsh-client-web/src/boot-client
 */
import type { Context } from '@deepseek-ai/cordis';
import type { BootManifest, ClientModuleLoader } from '@deepseek-ai/dsh-client-modules/client';
import { STATE_LABELS } from './loader-status.ts';
/** Entry state label as the boot page renders it. */
export type EntryStateLabel = (typeof STATE_LABELS)[keyof typeof STATE_LABELS] | 'loading' | 'failed';
/** Inputs of {@link bootClient}. */
export interface ClientBootOptions {
    /** Fresh root Context that will own the plugin tree. */
    readonly ctx: Context;
    /** Module system installed as `loader.internal`. */
    readonly modules: ClientModuleLoader;
    /** Parsed manifest whose `plugins` rows become Loader entries (entry name = row id). */
    readonly manifest: BootManifest;
    /** Per-entry state reporting (the boot page); omitted when no one renders progress. */
    readonly onEntryState?: (name: string, state: EntryStateLabel) => void;
}
/**
 * Compose the client: `ctx.plugin(Loader)`, `loader.internal = modules`, one
 * `loader.create({ name })` per manifest row, `loader.await()`, then
 * {@link assertEntriesActive}. A row whose module cannot be imported is marked
 * failed; the Loader logs its import error, the module system records it, and
 * the audit reports that error text per entry.
 * @param options - context, module system, manifest, optional progress sink.
 * @returns resolves after the audit accepts the tree, including after optional-entry warnings;
 * rejects with the audit report when a required entry is inactive.
 */
export declare function bootClient(options: ClientBootOptions): Promise<void>;
/**
 * Audit entries that failed import/apply or still wait on missing services.
 * An inactive entry named in the required list rejects startup; other
 * inactive entries produce one warning and leave active siblings running.
 * A failed entry reports its recorded activation error; reading it awaits
 * only failed fibers, which have settled by the time the Loader has.
 * @param ctx - root Context carrying the Loader.
 * @param modules - the module system whose recorded import failures name why an entry
 *   has no fiber; a row with no record points at the console. An entry without a fiber
 *   failed to import or was rejected at creation (its module exports no plugin); like the
 *   Host audit's `failed to import`, both are reported as an import failure.
 * @param warn - sink for the optional-entry warning.
 * @returns resolves after any optional-entry warning.
 * @throws {Error} rejects when a required entry is inactive, listing every non-active entry with its
 *   reason and marking the required ones; no warning is written in that case.
 */
export declare function assertEntriesActive(ctx: Context, modules: Pick<ClientModuleLoader, 'importError'>, warn?: (message: string) => void): Promise<void>;
//# sourceMappingURL=boot-client.d.ts.map