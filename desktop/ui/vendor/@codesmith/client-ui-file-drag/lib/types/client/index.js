import { createSnapshotStore } from '@codesmith/client-store';
import { FileDragFeedback } from "./feedback.js";
import { FileTransfers } from "./transfers.js";
import { en, ja, es, fr, de, ru, pt, zh } from "./locales.js";
/** Registry and locale used by the independent feedback overlay. */
export const inject = ['slots', 'locale'];
/** @param ctx - client plugin context carrying slots and locale. */
export function apply(ctx) {
    const bridge = globalThis.cdsDesktop;
    const desktop = bridge?.protocolVersion === 1 ? bridge.fileDrag : undefined;
    const failure = createSnapshotStore(null);
    const t = ctx.locale.bind('fileDrag');
    ctx.effect(() => ctx.locale.register('fileDrag', { zh, en, ja, es, fr, de, ru, pt }));
    let sequence = 0;
    const reportFailure = () => { failure.set({ text: t('failed'), sequence: ++sequence }); };
    const transfers = new FileTransfers(desktop, reportFailure);
    const dismiss = () => { failure.set(null); };
    ctx.provide('fileDrag', {
        actions: { prepare: (source, signal) => transfers.prepare(source, signal), reportFailure },
        read: transfer => transfers.read(transfer),
    });
    ctx.effect(() => () => { transfers.dispose(); dismiss(); });
    ctx.slots.inject('shell.overlay', () => ctx.slots.register({
        name: 'shell.overlay', id: 'file-drag.feedback',
        inject: () => ({ hooks: { failure }, dismiss }),
    }, FileDragFeedback));
}
//# sourceMappingURL=index.js.map