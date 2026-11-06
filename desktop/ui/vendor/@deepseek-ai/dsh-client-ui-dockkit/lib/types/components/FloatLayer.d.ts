import type { CSSProperties, PointerEvent as ReactPointerEvent, ReactNode } from 'react';
import type { DockIntents, DockLabels, TabRenderer } from '../contract/adapter.ts';
import type { FloatRect, LayoutState, PaneId, TabId, TabRecord } from '../contract/types.ts';
/** The layout whose `floats` this layer draws. */
export interface FloatLayerProps {
    readonly state: LayoutState;
    readonly intents: DockIntents;
    readonly labels: DockLabels;
    readonly renderTab: TabRenderer;
    /** Whether a floating tab offers its close control; defaults to true. Called per tab on every render. */
    readonly canCloseTab?: (tabId: TabId) => boolean;
    /** The panel header's title content; omit to show the record's `title` text (see `DockSurfaceProps`). */
    readonly renderTabTitle?: TabRenderer;
}
/**
 * Inline position of a floating panel. A gesture or return draws it exactly at
 * `rect`. At rest the panel only publishes `rect` to the `.float` rule, which
 * clamps it into the viewport and below the caption floor, so a stale viewport
 * reading in `useFloatGestures` still leaves the panel inside.
 * @param rect - the rectangle to draw.
 * @param lifted - whether a gesture or return is drawing the panel.
 * @returns the panel's inline position and size.
 */
export declare function floatStyle(rect: FloatRect, lifted: boolean): CSSProperties;
/**
 * Shared move/resize gestures; changing layout mode never owns the content DOM.
 * Panels draw at their stored rectangle projected into the viewport, so a
 * shrinking window pushes them in without recording anything. A move past an
 * edge draws with resistance; its release records the legal rectangle and the
 * panel springs there from where it was drawn.
 * @param state - committed layout and float stacking order.
 * @param intents - gesture settlements.
 * @returns the current preview, the rectangle to draw per panel, and focus/gesture callbacks.
 */
export declare function useFloatGestures(state: LayoutState, intents: DockIntents): {
    preview: {
        paneId: PaneId;
        rect: FloatRect;
    } | undefined;
    raise: (paneId: PaneId) => void;
    drag: (mode: "move" | "resize", paneId: PaneId, event: ReactPointerEvent<HTMLElement>) => void;
    rectOf: (paneId: PaneId, stored: FloatRect) => FloatRect;
};
/**
 * Header-only controls, usable above either retained or visibility-mounted bodies.
 * @param props - owning pane/tab, localized labels and gesture callbacks.
 * @returns the float header without a content container.
 */
export declare function FloatHeader({ paneId, tab, labels, intents, renderTabTitle, canCloseTab, drag }: {
    readonly paneId: PaneId;
    readonly tab: TabRecord;
    readonly labels: DockLabels;
    readonly intents: DockIntents;
    readonly renderTabTitle: TabRenderer | undefined;
    readonly canCloseTab: ((id: TabId) => boolean) | undefined;
    readonly drag: ReturnType<typeof useFloatGestures>['drag'];
}): ReactNode;
/** Every floating panel, in z order. */
export declare function FloatLayer({ state, intents, labels, renderTab, renderTabTitle, canCloseTab }: FloatLayerProps): ReactNode;
//# sourceMappingURL=FloatLayer.d.ts.map