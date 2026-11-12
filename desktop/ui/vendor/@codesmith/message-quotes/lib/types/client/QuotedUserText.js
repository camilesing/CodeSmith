import { jsx as _jsx } from "react/jsx-runtime";
/** Literal display for text-derived quote nodes. */
import { QuotePreview } from '@codesmith/client-ui-primitives';
/**
 * Display decoded quote text without further interpretation.
 * @param props - the quote node produced by the registered blockquote matcher.
 * @returns the shared literal excerpt and clipped full-text popover.
 */
export function QuotedUserText({ node }) {
    return _jsx(QuotePreview, { text: node.data.text });
}
//# sourceMappingURL=QuotedUserText.js.map