/** Physical socket selection for browser, desktop, and authenticated Node connections. */
/**
 * Resolve a logical channel against the explicit Host or the page's document base.
 * @param path - absolute logical channel path.
 * @param factory - authenticated socket factory; omission uses the browser API.
 * @param baseUrl - Host base URL, required outside a document.
 * @returns a socket owned by the calling channel.
 */
export function openConnectionWebSocket(path, factory = url => new WebSocket(url), baseUrl) {
    const base = baseUrl ?? (typeof document === 'undefined' ? undefined : document.baseURI);
    if (base === undefined)
        throw new Error('connection: WebSocket carrier requires streamBaseUrl outside a document');
    const url = new URL(path.slice(1), base);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    return factory(url.href);
}
//# sourceMappingURL=socket.js.map