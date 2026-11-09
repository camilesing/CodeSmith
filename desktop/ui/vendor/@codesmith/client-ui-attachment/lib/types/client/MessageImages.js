import { jsx as _jsx } from "react/jsx-runtime";
/** Official image gallery Factory and default image Slot adapter. */
import { useMemo } from 'react';
import { ImageGallery } from "../MessageImage.js";
import { messageImageLabels } from "./labels.js";
/**
 * Render a gallery with an authorized loader and caller-owned or default labels.
 * @param props - image sources, authorized loader, layout hints, optional labels, and Factory locale seat.
 * @returns the official gallery; no Session binding is needed by this component.
 */
export function MessageImagesContent({ images, loadImage, align, compact = false, thumbnail = false, labels, t, fileDrag, useContextMenu }) {
    const { getTriggerProps } = useContextMenu();
    const resolved = useMemo(() => labels ?? messageImageLabels(t), [labels, t]);
    return _jsx(ImageGallery, { images: images, load: loadImage, align: align, compact: compact, thumbnail: thumbnail, labels: resolved, fileDrag: fileDrag, getTriggerProps: getTriggerProps });
}
/**
 * Fill a message, trajectory, or tool image position through the shared Content Factory.
 * @param props - Slot owner image data, authorized loader, and framework Factory renderer.
 * @returns the official gallery occurrence.
 */
export function MessageImages({ images, loadImage, align, compact, thumbnail, renderFactorySlot }) {
    return renderFactorySlot('conversation.images.content', {
        images, loadImage, align,
        ...compact === undefined ? {} : { compact },
        ...thumbnail === undefined ? {} : { thumbnail },
    });
}
//# sourceMappingURL=MessageImages.js.map