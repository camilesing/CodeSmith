import { jsx as _jsx } from "react/jsx-runtime";
import { toolFileInject } from "../file-menu.js";
import { IconEditOutlineRegular } from '@codesmith/client-ui-primitives';
import { diffCardModel } from "../models/diff-card-model.js";
import { toolRowModel } from "../models/tool-call-model.js";
import { ToolRow } from "../components/ToolRow.js";
import { CONVERSATION_NS as NS } from "../../locale.js";
const FILE_MUTATION_ICON = _jsx(IconEditOutlineRegular, { size: 14 });
const CONTENT_FIELDS = ['content', 'old_string', 'new_string'];
const KILOBYTE = 1024;
/** Kilobytes of decoded input at every stage; null before any content field arrives. */
function contentKilobytes(block) {
    const { args } = block;
    const open = CONTENT_FIELDS.find(key => args.has(key) && !args.complete(key));
    const completed = CONTENT_FIELDS.reduce((total, key) => key !== open && args.complete(key)
        ? total + (args.stringLength(key, { step: KILOBYTE }) ?? 0) : total, 0);
    if (open === undefined) {
        return CONTENT_FIELDS.some(key => args.has(key)) ? Math.ceil(completed / KILOBYTE) : null;
    }
    const chars = completed + (args.stringLength(open, { step: KILOBYTE, offset: completed }) ?? 0);
    return Math.ceil(chars / KILOBYTE);
}
/**
 * Shows the path and decoded input size through preparation, execution, and
 * settlement, with the applied diff available once the call settles.
 */
export function FileMutationRow({ toolName, block, cwd, home, openFile, useContextMenu, sessionId, resolveFilePath, inspect, useDisclosure, t, }) {
    const { getTriggerProps } = useContextMenu();
    const model = toolRowModel(toolName, block, cwd, home);
    const path = model.filePath;
    const diff = diffCardModel(block);
    const kilobytes = contentKilobytes(block);
    const size = kilobytes === null ? null : t('tool.preparing.content', { kilobytes });
    return (_jsx(ToolRow, { fileContextMenuProps: getTriggerProps(path === undefined ? undefined : () => ({
            kind: 'file', value: { sessionId, path: resolveFilePath(path) },
        })), useDisclosure: useDisclosure, t: t, variant: model.variant, toolName: toolName, icon: FILE_MUTATION_ICON, title: t(model.titleKey), summary: model.summary, summarySuffix: size !== null && model.summary !== '' ? size : undefined, output: model.output, errorSummary: model.errorSummary, diff: diff, state: model.state, filePath: model.filePath, onOpenFile: openFile, inspect: inspect }));
}
/** Registers file-mutation conversation rows. */
export const fileMutationToolview = {
    name: 'file-mutation-toolview',
    inject: ['slots', 'sessions'],
    apply(ctx) {
        ctx.slots.inject('tool.call.toolview', function* () {
            yield ctx.slots.register({ name: 'tool.call.toolview', key: 'apply_patch', locale: NS, inject: toolFileInject(ctx) }, FileMutationRow);
            yield ctx.slots.register({ name: 'tool.call.toolview', key: 'edit', locale: NS, inject: toolFileInject(ctx) }, FileMutationRow);
            yield ctx.slots.register({ name: 'tool.call.toolview', key: 'write', locale: NS, inject: toolFileInject(ctx) }, FileMutationRow);
        });
    },
};
//# sourceMappingURL=file-mutation-row.js.map