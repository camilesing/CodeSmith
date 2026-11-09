/** Plain text implementation registered through the same document extension points as other viewers. */
import type { Context } from '@codesmith/cordis';
import type { DocumentPreviewDefinition } from '../document/registry.ts';
/** Stable plain-text implementation identity within this package. */
export declare const PLAIN_BODY_ID = "@codesmith/client-ui-sidebar-documentpreview/text";
/**
 * Describe the plain-text fallback.
 * @param title - locale-owned implementation name.
 * @returns plain-text registration metadata.
 */
export declare function textBodyDefinition(title: () => string): DocumentPreviewDefinition;
/** @param ctx - owning plugin context. Register the fallback metadata and keyed body. */
export declare function apply(ctx: Context): void;
//# sourceMappingURL=index.d.ts.map