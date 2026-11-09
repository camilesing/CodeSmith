/** Builtin Markdown metadata and keyed document-body registration. */
import type { Context } from '@codesmith/cordis';
import type { DocumentPreviewDefinition } from '../document/registry.ts';
/** Implementation identity shared by metadata and the document slot. */
export declare const MARKDOWN_BODY_ID = "@codesmith/client-ui-sidebar-documentpreview/markdown";
/**
 * Describe the Markdown implementation without taking ownership of loading.
 * @param title - locale-owned implementation name.
 * @returns builtin Markdown registration metadata.
 */
export declare function markdownDefinition(title: () => string): DocumentPreviewDefinition;
/**
 * Register locale, metadata, and the document body for the owning plugin lifetime.
 * @param ctx - plugin context carrying locale, document registry, and slots.
 */
export declare function apply(ctx: Context): void;
//# sourceMappingURL=index.d.ts.map