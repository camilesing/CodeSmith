import { ICON_SYMBOLS } from '@deepseek-ai/dsh-client-ui-primitives';
const SVG_NS = 'http://www.w3.org/2000/svg';
/** Root attributes that size or label one rendering, not the reusable artwork. */
const DROPPED_ATTRIBUTES = new Set(['width', 'height', 'xmlns', 'class', 'aria-hidden']);
function toSymbol(id, markup) {
    const root = new DOMParser().parseFromString(markup, 'image/svg+xml').documentElement;
    const symbol = document.createElementNS(SVG_NS, 'symbol');
    for (const { name, value } of root.attributes)
        if (!DROPPED_ATTRIBUTES.has(name))
            symbol.setAttribute(name, value);
    symbol.id = id;
    symbol.append(...[...root.childNodes].map(node => document.importNode(node, true)));
    return symbol;
}
/**
 * Mount the sprite and keep each symbol on the active theme's replacement or
 * the ui-primitives default; plugin disposal removes the sprite.
 * @param ctx - owning plugin context; `theme/change` snapshots drive updates.
 * @param icons - replacements of the theme active at install time.
 */
export function installIconSprite(ctx, icons) {
    if (typeof document === 'undefined')
        return;
    const sprite = document.createElementNS(SVG_NS, 'svg');
    sprite.setAttribute('aria-hidden', 'true');
    sprite.setAttribute('data-dsh-icon-sprite', '');
    sprite.style.cssText = 'position: absolute; width: 0; height: 0; overflow: hidden';
    const rendered = new Map();
    const markups = new Map();
    const apply = (replacements = {}) => {
        for (const [id, fallback] of Object.entries(ICON_SYMBOLS)) {
            const markup = replacements[id] ?? fallback;
            if (markups.get(id) === markup)
                continue;
            const symbol = toSymbol(id, markup);
            const previous = rendered.get(id);
            if (previous === undefined)
                sprite.append(symbol);
            else
                previous.replaceWith(symbol);
            rendered.set(id, symbol);
            markups.set(id, markup);
        }
    };
    apply(icons);
    ctx.effect(() => {
        document.body.prepend(sprite);
        return () => { sprite.remove(); };
    }, 'ui-theme: icon sprite');
    ctx.on('theme/change', (snapshot) => { apply(snapshot.active.icons); });
}
//# sourceMappingURL=icon-sprite.js.map