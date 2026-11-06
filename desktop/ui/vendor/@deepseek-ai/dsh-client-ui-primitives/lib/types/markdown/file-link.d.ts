/** Local Markdown destinations accepted by the file-preview callback. */
/**
 * Recognize a relative, rooted, or absolute Windows path before URL parsing.
 * @param value - A decoded or URI-normalized destination.
 * @returns Whether a filesystem prefix makes the destination unambiguous.
 */
export declare function isExplicitFilePath(value: string): boolean;
/**
 * Decode a file destination with a fragment, line range, or line/column suffix.
 * Explicit relative, slash/backslash-rooted, and Windows drive paths take
 * precedence over URLs. Other parseable absolute URLs are not file links.
 * Fragments take precedence over colon suffixes; invalid locations open the
 * file without selecting a line. Columns are consumed but not selected.
 * Percent-encode literal `?`, `#`, and numeric colon suffixes in filenames.
 * @param value - Parsed Markdown link destination.
 * @returns A local path and optional first line, or undefined for URLs,
 * fragment-only links, queries, malformed escapes, or control characters.
 */
export declare function parseFileLink(value: string): {
    path: string;
    line?: number;
} | undefined;
//# sourceMappingURL=file-link.d.ts.map