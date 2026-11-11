import { jsx as _jsx } from "react/jsx-runtime";
import { ReferenceChip } from '@codesmith/client-ui-primitives';
/**
 * Render one confirmed command token without adding an activation action.
 * @param props - resolved command node and its original spelling.
 * @returns a noninteractive inline command chip.
 */
export function GoalCommandReference({ node, raw }) {
    return _jsx(ReferenceChip, { appearance: "command", label: `/${node.data.name}`, title: raw });
}
//# sourceMappingURL=GoalCommandReference.js.map