/** Synchronous placement for body-portaled panels with a viewport fixed-position containing block. */
import { type CSSProperties, type RefObject } from 'react';
/** Inputs for {@link useAnchoredPosition}. */
export interface AnchoredPositionOptions {
    /** Whether the panel is mounted and should track its anchor. */
    open: boolean;
    /** Element whose viewport rect anchors the panel. */
    anchorRef: RefObject<HTMLElement | null>;
    /** Body-portaled, fixed-position panel; its ancestors must not establish a containing block. */
    panelRef: RefObject<HTMLElement | null>;
    /** Preferred anchor edge; right-side panels align their top edges. */
    side?: 'top' | 'bottom' | 'right';
    /** Horizontal alignment for panels above or below the anchor. */
    align?: 'start' | 'end';
    /** Distance from the preferred anchor edge, in CSS pixels. */
    gap: number;
    /** Minimum viewport inset; desktop frame clearance can increase the top inset. */
    margin: number;
    /** Reserve desktop frame clearance in addition to the margin; false keeps only the viewport margin. Defaults to true. */
    frameClearance?: boolean;
    /** Host-supplied client rect; null hides the panel until the anchor is available. */
    getAnchorRect?: () => DOMRect | null;
    /** Re-measure a committed content change before paint. */
    layoutKey?: string | number;
    /** Minimum panel width follows the anchor, bounded by the visible viewport. */
    minWidthFromAnchor?: boolean;
}
/**
 * Keep a fixed panel aligned and constrained to the visible viewport.
 * Panel CSS consumes `--cds-anchored-max-width` and `--cds-anchored-max-height` for size limits.
 * The hook owns those inline variables and optional min-width while open, restoring them on cleanup.
 * @param options - Open state, element refs, placement, and optional host rect or content change key.
 * @returns Fixed left/top coordinates, or null while closed or awaiting an anchor; the first measurement completes before paint.
 */
export declare function useAnchoredPosition(options: AnchoredPositionOptions): CSSProperties | null;
//# sourceMappingURL=useAnchoredPosition.d.ts.map