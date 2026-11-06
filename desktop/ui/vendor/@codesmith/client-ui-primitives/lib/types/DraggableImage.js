import { jsx as _jsx } from "react/jsx-runtime";
/** Original-image drag gestures owned by a presentation primitive. */
import { forwardRef } from 'react';
import { useImageDrag } from "./image-drag.js";
/**
 * Render an image with original-file dragging; using explicit application-owned callbacks.
 * @param props - image attributes, original filename and file preparation callbacks.
 * @param ref - image element for measurement and pointer-mode bindings.
 * @returns a draggable image that suppresses only the mouse click following its drag.
 */
export const DraggableImage = forwardRef(function DraggableImage({ fileDrag, src, name, original, resourceAddress, ...props }, ref) {
    const drag = useImageDrag(fileDrag, src, name, original, resourceAddress);
    return _jsx("img", { ...props, ...drag, src: src, ref: ref, draggable: true });
});
//# sourceMappingURL=DraggableImage.js.map