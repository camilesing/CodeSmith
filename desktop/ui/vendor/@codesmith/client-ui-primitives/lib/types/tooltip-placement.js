const opposite = { right: 'left', left: 'right', top: 'bottom', bottom: 'top' };
/**
 * Prefer the requested side, its opposite, then perpendicular sides without covering the anchor.
 * @param input - Observed dimensions, anchor edges, viewport and requested placement; horizontal gaps remain 10px.
 * @returns Border-box coordinates and the selected side; if no side fits, a viewport-bounded fallback.
 */
export function placeTooltip(input) {
    const { anchor, viewportWidth, viewportHeight, side, align, gap } = input;
    let { width, height } = input;
    const margin = 12;
    const maxWidth = Math.max(0, viewportWidth - margin * 2);
    const maxHeight = Math.max(0, viewportHeight - margin * 2);
    width = Math.min(width, maxWidth);
    height = Math.min(height, maxHeight);
    const clampX = (x) => Math.max(margin, Math.min(x, viewportWidth - margin - width));
    const clampY = (y) => Math.max(margin, Math.min(y, viewportHeight - margin - height));
    const horizontal = side === 'right' || side === 'left';
    const candidates = [side, opposite[side], ...horizontal ? ['bottom', 'top'] : ['right', 'left']];
    const positions = {
        right: { left: anchor.right + 10, top: clampY((anchor.top + anchor.bottom - height) / 2) },
        left: { left: anchor.left - 10 - width, top: clampY((anchor.top + anchor.bottom - height) / 2) },
        bottom: { left: clampX(align === 'end' ? anchor.right - width : (anchor.left + anchor.right - width) / 2), top: anchor.bottom + gap },
        top: { left: clampX(align === 'end' ? anchor.right - width : (anchor.left + anchor.right - width) / 2), top: anchor.top - gap - height },
    };
    for (const candidate of candidates) {
        const position = positions[candidate];
        if (position.left >= margin && position.top >= margin
            && position.left + width <= viewportWidth - margin && position.top + height <= viewportHeight - margin) {
            return { ...position, side: candidate, width, height, overflow: false };
        }
    }
    const position = positions[side];
    return { left: clampX(position.left), top: clampY(position.top), side, width, height, overflow: true };
}
//# sourceMappingURL=tooltip-placement.js.map