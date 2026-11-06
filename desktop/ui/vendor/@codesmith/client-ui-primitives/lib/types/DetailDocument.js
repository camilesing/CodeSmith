import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useMemo } from 'react';
import { Button } from "./Button.js";
import { MarkdownText } from "./markdown/MarkdownText.js";
import { StateDot } from "./StateDot.js";
import css from './DetailDocument.module.css';
/**
 * The state {@link DetailDocument} shows for an owner's read: loading until it settles, then the document, none, or the
 * failure.
 * @param read - the owner's read; undefined before it starts.
 * @returns the state to show.
 */
export function detailDocumentState(read) {
    if (read?.status === 'ready')
        return read.markdown === undefined ? { status: 'none' } : { status: 'found', markdown: read.markdown };
    return read?.status === 'error' ? read : { status: 'loading' };
}
/**
 * Assemble reference-stable labels for {@link DetailDocument} from translated strings.
 * @param copy - the translated strings.
 * @returns labels that keep their identity while the strings stay equal.
 */
export function useDetailDocumentLabels(copy) {
    const { title, loading, retry, footnotes } = copy;
    const { copy: copyLabel, copied: copiedLabel, codeBlock: codeLabel, wrap: wrapLabel, unwrap: unwrapLabel } = copy;
    const markdown = useMemo(() => ({ code: { copyLabel, copiedLabel, toolbarLabels: { codeLabel, wrapLabel, unwrapLabel } }, footnotes }), [copyLabel, copiedLabel, codeLabel, wrapLabel, unwrapLabel, footnotes]);
    return useMemo(() => ({ title, loading, retry, markdown }), [title, loading, retry, markdown]);
}
/**
 * A section showing a long Markdown description, without a heading of its own: three skeleton lines while it loads, the
 * document once found, or the owner's failure message with a retry. Nothing renders for `none`. A document taller than
 * the section's height limit scrolls inside it, and the scroll region, named by `labels.title`, takes keyboard focus.
 * Raw HTML is omitted, relative links stay text, and only images the owner's `images` resolver accepts, or absolute
 * HTTP(S) images, load.
 * @param props.state - the read to show.
 * @param props.labels - localized region name, placeholder name, retry text, and Markdown chrome; keep `markdown` reference-stable.
 * @param props.images - resolves the document's relative image destinations to displayable URLs.
 * @param props.onRetry - reads the document again after a failure.
 * @returns the section, or null.
 */
export function DetailDocument({ state, labels, images, onRetry }) {
    if (state.status === 'none')
        return null;
    let body;
    switch (state.status) {
        case 'found':
            body = (_jsx("div", { className: css.document, role: "region", "aria-label": labels.title, tabIndex: 0, children: _jsx(MarkdownText, { text: state.markdown, labels: labels.markdown, pathImages: images, omitHtml: true }) }));
            break;
        case 'error':
            body = (_jsxs("div", { className: css.failure, children: [_jsxs("p", { className: css.error, role: "alert", children: [_jsx(StateDot, { state: "error" }), state.message] }), _jsx(Button, { variant: "outline", size: "sm", onClick: onRetry, children: labels.retry })] }));
            break;
        case 'loading':
            body = (_jsxs("div", { className: css.skeleton, role: "status", "aria-label": labels.loading, "aria-busy": "true", children: [_jsx("span", { className: `${css.bar} ${css.long}` }), _jsx("span", { className: `${css.bar} ${css.long}` }), _jsx("span", { className: `${css.bar} ${css.short}` })] }));
            break;
    }
    return (_jsx("section", { className: css.section, "data-detail-document": true, children: body }));
}
//# sourceMappingURL=DetailDocument.js.map