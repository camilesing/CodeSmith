import { jsx as _jsx } from "react/jsx-runtime";
import { ReferenceChip } from '@codesmith/client-ui-primitives';
/**
 * Render a skill chip; previews omit activation even when the entry has an injected action.
 * @param props - resolved node, raw token, interaction mode, and Session-bound preview action.
 * @returns an inline skill label or interactive chip.
 */
export function SkillReference({ node, raw, interactive, openSkill }) {
    return _jsx(ReferenceChip, { appearance: "skill", label: `/${node.data.name}`, title: raw, ...(interactive ? { onActivate: () => { openSkill(node.data.name); } } : {}) });
}
//# sourceMappingURL=SkillReference.js.map