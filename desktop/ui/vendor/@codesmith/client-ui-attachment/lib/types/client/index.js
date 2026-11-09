import { ComposerAttachments } from "./ComposerAttachments.js";
import { MessageImages, MessageImagesContent } from "./MessageImages.js";
/** Slot registry required by the attachment presentation plugin. */
export const inject = ['slots', 'fileDrag'];
/**
 * Register composer attachments and the shared gallery with its domain adapters.
 * @param ctx - client plugin context with the Slot registry.
 */
export function apply(ctx) {
    ctx.slots.registerFactory({
        name: 'conversation.images.content', scope: 'root', locale: 'conversation',
        inject: () => ({
            fileDrag: ctx.fileDrag.actions,
        }),
    }, MessageImagesContent);
    ctx.slots.inject('conversation.input.attachments', () => ctx.slots.register({
        name: 'conversation.input.attachments',
        inject: () => ({ readFileDrag: ctx.fileDrag.read }),
        locale: 'conversation',
    }, ComposerAttachments));
    ctx.slots.inject('conversation.chat.message.images', () => ctx.slots.register({
        name: 'conversation.chat.message.images', priority: 100,
    }, MessageImages));
    ctx.slots.inject('conversation.chat.user.images', () => ctx.slots.register({
        name: 'conversation.chat.user.images', priority: 100,
    }, MessageImages));
    ctx.slots.inject('conversation.trajectory.images', () => ctx.slots.register({
        name: 'conversation.trajectory.images', priority: 100,
    }, MessageImages));
    ctx.slots.inject('tool.call.images', () => ctx.slots.register({
        name: 'tool.call.images', priority: 100,
    }, MessageImages));
}
//# sourceMappingURL=index.js.map