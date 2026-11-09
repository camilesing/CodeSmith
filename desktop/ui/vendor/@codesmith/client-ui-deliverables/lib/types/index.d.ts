/**
 * Deliverables plugin, node half. Serves authenticated native opens of declared files. The browser
 * half ships via exports["./client"], discovered through the package.json
 * cds.client declaration.
 */
import type { Context } from '@codesmith/cordis';
/** Services required for change summaries and authenticated native opens. */
export declare const inject: string[];
/**
 * Register native opens for declared files.
 * @param ctx - host context carrying file and connection services.
 */
export declare function apply(ctx: Context): void;
//# sourceMappingURL=index.d.ts.map