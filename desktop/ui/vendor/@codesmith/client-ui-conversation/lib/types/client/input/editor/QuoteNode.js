import { jsx as _jsx } from "react/jsx-runtime";
import { QuotePreview } from '@codesmith/client-ui-primitives';
import { formatQuoteText } from '@codesmith/client-ui-primitives';
import { createCommand, DecoratorNode } from 'lexical';
const EMPTY_LABELS = { remove: '' };
/** Remove one quote occurrence by its Lexical identity. */
export const REMOVE_QUOTE_COMMAND = createCommand('REMOVE_QUOTE_COMMAND');
/** Pure React face of one quote node. */
export function QuoteNodeView({ editor, nodeKey, text, labels }) {
    return (_jsx(QuotePreview, { text: text, removal: {
            label: labels.remove,
            onRemove: () => { editor.dispatchCommand(REMOVE_QUOTE_COMMAND, nodeKey); },
        } }));
}
/** One selected transcript range represented as an atomic inline decorator. */
export class QuoteNode extends DecoratorNode {
    __text;
    __labels;
    /** Lexical node registry type tag. */
    static getType() {
        return 'quote';
    }
    /** @param node - node to clone. @returns a copy carrying the same NodeKey. */
    static clone(node) {
        return new QuoteNode(node.__text, node.__labels, node.__key);
    }
    /** @param json - serialized editor node. @returns a fresh node with default runtime labels. */
    static importJSON(json) {
        return new QuoteNode(json.text);
    }
    /**
     * @param text - selected text.
     * @param labels - runtime-only localized labels.
     * @param key - Lexical clone-path key; absent for fresh nodes.
     */
    constructor(text, labels = EMPTY_LABELS, key) {
        super(key);
        this.__text = text;
        this.__labels = labels;
    }
    /** Serialize the quote data without its runtime presentation state. */
    exportJSON() {
        return {
            ...super.exportJSON(),
            type: 'quote',
            version: 1,
            text: this.__text,
        };
    }
    /** Mount one non-editable inline host. */
    createDOM(_config) {
        const element = document.createElement('span');
        element.setAttribute('data-composer-quote', '');
        element.setAttribute('contenteditable', 'false');
        return element;
    }
    /** Updates reuse the existing host element. */
    updateDOM() {
        return false;
    }
    /** Quotes flow with draft text and wrap at their own text boundary. */
    isInline() {
        return true;
    }
    /** Arrow movement crosses the quote without entering a NodeSelection. */
    isKeyboardSelectable() {
        return false;
    }
    /** Clipboard text is the exact model-visible Markdown blockquote. */
    getTextContent() {
        return formatQuoteText(this.__text);
    }
    /** Selected visible quote text. */
    getQuoteText() {
        return this.getLatest().__text;
    }
    /** Update the localized remove label without changing persisted quote data. */
    setLabels(labels) {
        if (this.getLatest().__labels.remove !== labels.remove)
            this.getWritable().__labels = labels;
    }
    /** Render the quote body and its independent close control. */
    decorate(editor) {
        return (_jsx(QuoteNodeView, { editor: editor, nodeKey: this.__key, text: this.__text, labels: this.__labels }));
    }
}
/** @param text - selected quote text. @param labels - runtime labels. @returns a fresh node. */
export function $createQuoteNode(text, labels) {
    return new QuoteNode(text, labels);
}
/** @param node - possible Lexical node. @returns whether it is a QuoteNode. */
export function $isQuoteNode(node) {
    return node instanceof QuoteNode;
}
//# sourceMappingURL=QuoteNode.js.map