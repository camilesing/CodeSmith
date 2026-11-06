import type { ReactNode } from 'react';
import type { LayoutState, PaneId, PaneNode } from '../contract/types.ts';
import type { PaneCallbacks } from './render.ts';
/** A pane and the live layout it reads its tabs from. */
export interface TabPanelProps {
    readonly state: LayoutState;
    readonly pane: PaneNode;
    readonly callbacks: PaneCallbacks;
}
/** A tab strip and where it reads and records its pane's chip-box offset. */
interface TabStripProps extends TabPanelProps {
    /**
     * Chip-box offsets by pane, shared by the strips that replace one another
     * as a pane's selected tab changes. Keys are valid within one layout only,
     * and entries outlive their panes. Omitted where no strip continues from
     * another's position.
     */
    readonly offsets?: Map<PaneId, number>;
}
/** @param props - pane state, gestures and shared offsets. @returns its tab strip without a content container. */
export declare function TabStrip({ state, pane, callbacks, offsets }: TabStripProps): ReactNode;
/** @param props - target pane and drag preview. @returns body-local drop feedback, or nothing. */
export declare function PaneDropHints({ pane, callbacks }: Pick<TabPanelProps, 'pane' | 'callbacks'>): ReactNode;
/** @param props - pane state and gestures. @returns one pane in the recursive, visibility-mounted layout. */
export declare function TabPanel({ state, pane, callbacks }: TabPanelProps): ReactNode;
export {};
//# sourceMappingURL=TabPanel.d.ts.map