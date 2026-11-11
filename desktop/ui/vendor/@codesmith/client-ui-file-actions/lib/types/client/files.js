/** Original-byte transfer, platform save selection, and pasteable image encoding. */
import { resolveDragFileName } from '@codesmith/util-file-drag';
/**
 * Keep compatible filename suffixes and repair known MIME conflicts without changing bytes.
 * @param name - suggested original name.
 * @param mediaType - source MIME, optionally including parameters.
 * @returns safe filename preserving unknown formats.
 */
export function exportName(name, mediaType) {
    return resolveDragFileName({ name: name ?? 'image', mediaType });
}
/** File action owner; browser download requests never imply a saved path. */
export class BrowserFileActions {
    desktop;
    downloadUrl;
    lifetime = new AbortController();
    canSaveAs;
    constructor(desktop) {
        this.desktop = desktop;
        this.canSaveAs = desktop !== undefined;
    }
    async read(source) {
        this.lifetime.signal.throwIfAborted();
        if (source.read !== undefined)
            return source.read(this.lifetime.signal);
        const url = new URL(source.url);
        if (!['http:', 'https:', 'blob:', 'data:', 'cds-app:'].includes(url.protocol))
            throw new Error('Unsupported file source');
        if (this.desktop !== undefined && ['http:', 'https:'].includes(url.protocol) && url.origin !== location.origin) {
            const content = await this.desktop.readClipboardImage(url.href);
            this.lifetime.signal.throwIfAborted();
            return new Blob([new Uint8Array(content.bytes)], { type: content.mediaType || source.mediaType || '' });
        }
        const response = await fetch(source.url, { signal: this.lifetime.signal, referrerPolicy: 'no-referrer' });
        if (!response.ok)
            throw new Error('Original file unavailable');
        return response.blob();
    }
    async save(source, mode) {
        this.lifetime.signal.throwIfAborted();
        if (this.desktop !== undefined) {
            const blob = source.read === undefined ? undefined : await source.read(this.lifetime.signal);
            this.lifetime.signal.throwIfAborted();
            const url = blob === undefined ? source.url : URL.createObjectURL(blob);
            try {
                const result = await this.desktop.save({ url, mode,
                    ...(source.name === undefined ? {} : { name: source.name }),
                    ...(blob?.type || source.mediaType ? { mediaType: blob?.type || source.mediaType } : {}),
                });
                this.lifetime.signal.throwIfAborted();
                return result;
            }
            finally {
                if (blob !== undefined)
                    URL.revokeObjectURL(url);
            }
        }
        if (mode === 'saveAs')
            throw new Error('Save as requires Desktop');
        const suggested = source.name ?? (URL.canParse(source.url) ? new URL(source.url).pathname.split('/').pop() : undefined);
        const blob = await this.read(source);
        this.lifetime.signal.throwIfAborted();
        const name = exportName(suggested, blob.type || source.mediaType || '');
        const url = URL.createObjectURL(blob);
        if (this.downloadUrl !== undefined)
            URL.revokeObjectURL(this.downloadUrl);
        this.downloadUrl = url;
        const anchor = document.createElement('a');
        anchor.href = url;
        anchor.download = name;
        document.body.append(anchor);
        anchor.click();
        anchor.remove();
        return { status: 'requested' };
    }
    async copyImage(source) {
        this.lifetime.signal.throwIfAborted();
        const png = this.read(source).then(async (blob) => {
            this.lifetime.signal.throwIfAborted();
            if (blob.type === 'image/png')
                return blob;
            const objectUrl = URL.createObjectURL(blob);
            const image = new Image();
            image.src = objectUrl;
            await image.decode().catch((error) => { URL.revokeObjectURL(objectUrl); throw error; });
            try {
                const canvas = document.createElement('canvas');
                canvas.width = image.naturalWidth;
                canvas.height = image.naturalHeight;
                const context = canvas.getContext('2d');
                if (context === null)
                    throw new Error('Image conversion unavailable');
                context.drawImage(image, 0, 0);
                return await new Promise((resolve, reject) => {
                    canvas.toBlob((value) => {
                        if (value === null)
                            reject(new Error('Image conversion failed'));
                        else
                            resolve(value);
                    }, 'image/png');
                });
            }
            finally {
                URL.revokeObjectURL(objectUrl);
            }
        });
        await navigator.clipboard.write([new ClipboardItem({ 'image/png': png })]);
    }
    /** Cancel reads and release browser download URLs when the plugin unloads. */
    dispose() {
        this.lifetime.abort();
        if (this.downloadUrl !== undefined)
            URL.revokeObjectURL(this.downloadUrl);
        this.downloadUrl = undefined;
    }
}
//# sourceMappingURL=files.js.map