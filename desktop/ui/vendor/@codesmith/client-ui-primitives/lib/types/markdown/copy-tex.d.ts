/** Clipboard projection that copies rendered KaTeX formulas as their TeX source. */
/**
 * Rewrite a copy whose selection lies inside one rendered Markdown document and
 * includes rendered math, so each formula becomes its trimmed TeX source,
 * delimited as `$…$` inline and `$$…$$` for display math, in both the
 * plain-text and HTML clipboard data. A selection endpoint inside a formula
 * extends to the whole formula in the clipboard only; the visible selection is
 * unchanged. A copy without math, a selection reaching outside `root`, and a
 * copy whose default an inner listener already prevented keep their data.
 * @param event - Native copy event.
 * @param root - The rendered Markdown document root.
 */
export declare function copyKatexAsTex(event: ClipboardEvent, root: HTMLElement): void;
//# sourceMappingURL=copy-tex.d.ts.map