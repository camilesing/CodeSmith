/**
 * A tab's body from the embedder's renderer, shared by every layout. A layout
 * re-renders for its own previews (a float gesture or return, a divider or tab
 * drag) on every frame; the body is memoized on the renderer and the tab
 * record, so such a frame leaves it untouched.
 */
import { memo } from 'react';
/** One tab's body, re-rendered only when `renderTab` or `tab` is a new object. */
export const TabBody = memo(function TabBody({ tab, renderTab }) {
    return renderTab(tab);
});
//# sourceMappingURL=TabBody.js.map