import { jsx as _jsx } from "react/jsx-runtime";
import { IconBrowseOutlineRegular } from '@codesmith/client-ui-primitives';
import { toolRowModel } from "../models/tool-call-model.js";
import { ToolRow } from "../components/ToolRow.js";
/**
 * Compose a read-family row: the shared chrome and model-derived fields, plus the
 * caller's card material.
 * @param props - the toolview runtime share and locale seat.
 * @param card - the card props this row owns.
 * @param getTriggerProps - ordinary menu bindings supplied by the toolview Slot.
 * @returns the assembled ToolRow.
 */
export function readFamilyRow({ toolName, block, cwd, home, openFile, resolveFilePath, sessionId, inspect, useDisclosure, t }, card, getTriggerProps) {
    const model = toolRowModel(toolName, block, cwd, home);
    const path = model.filePath;
    return (_jsx(ToolRow, { fileContextMenuProps: getTriggerProps(path === undefined ? undefined : () => ({
            kind: 'file', value: { sessionId, path: resolveFilePath(path),
                ...(card.filePathLine === undefined ? {} : { line: card.filePathLine }) },
        })), useDisclosure: useDisclosure, t: t, variant: model.variant, toolName: toolName, icon: _jsx(IconBrowseOutlineRegular, { size: 14 }), title: t(model.titleKey), summary: model.summary, bodyRaw: null, output: model.output, errorSummary: model.errorSummary, ...card, state: model.state, filePath: model.filePath, onOpenFile: openFile, inspect: inspect }));
}
//# sourceMappingURL=read-family-row.js.map