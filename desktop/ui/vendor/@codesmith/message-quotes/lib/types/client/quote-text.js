/** Explicit blockquote recognition and decoding for quote presentation. */
/**
 * Remove one blockquote marker from each explicitly quoted line.
 * @param text - consecutive blockquote lines, with up to three leading spaces.
 * @returns excerpt text with original line endings normalized.
 */
export function quoteTextOf(text) {
    return text.replace(/\r\n?/gu, '\n').replace(/^ {0,3}>[ \t]?/gmu, '');
}
/**
 * Recognize consecutive explicitly marked Markdown blockquote lines.
 * @param text - complete original message text.
 * @returns ordered nonblank quote spans; inline greater-than signs remain ordinary text.
 */
export function messageQuoteRanges(text) {
    const ranges = [];
    for (const match of text.matchAll(/^ {0,3}>[^\r\n]*(?:\r?\n {0,3}>[^\r\n]*)*/gmu)) {
        if (quoteTextOf(match[0]).trim() !== '')
            ranges.push({ start: match.index, end: match.index + match[0].length });
    }
    return ranges;
}
//# sourceMappingURL=quote-text.js.map