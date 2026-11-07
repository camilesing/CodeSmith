import { toolFileInject } from "../file-menu.js";
import { readCallLine, readCardModel } from "../models/read-card-model.js";
import { readFamilyRow } from "./read-family-row.js";
import { CONVERSATION_NS as NS } from "../../locale.js";
/**
 * Lets users expand a completed read result and open its reported path at the
 * line the call started from.
 */
export function ReadRow(props) {
    const { block, cwd, home } = props;
    const { getTriggerProps } = props.useContextMenu();
    return readFamilyRow(props, {
        read: readCardModel(block, cwd, home),
        filePathLine: readCallLine(block),
    }, getTriggerProps);
}
/** Registers the read tool's conversation row. */
export const readToolview = {
    name: 'read-toolview',
    inject: ['slots', 'sessions'],
    apply(ctx) {
        ctx.slots.inject('tool.call.toolview', () => ctx.slots.register({ name: 'tool.call.toolview', key: 'read', locale: NS, inject: toolFileInject(ctx) }, ReadRow));
    },
};
//# sourceMappingURL=read-row.js.map