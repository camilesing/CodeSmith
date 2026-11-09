/** Input-type declarations shared by the CodeSmith and pi-ai catalog editors. */
import type { ReactNode } from 'react';
import type { CodeSmithModelDraft } from './CodeSmithModelsEditor.tsx';
import type { ModelsKey } from './locales.ts';
/** Props of {@link ModelInputTypes}. */
interface ModelInputTypesProps {
    /** Effective model row, including fields outside the curated editor. */
    model: CodeSmithModelDraft;
    /** Adapter-owned field; pi-ai inherits capabilities when absent or empty. */
    field: 'inputModalities' | 'input';
    /** One-based row position for the accessible group label. */
    position: number;
    /** Prevent changes while read-only or saving. */
    disabled: boolean;
    /** Installed model or provider defaults when the row does not declare input types. */
    fallback?: readonly string[] | undefined;
    /** Section copy. */
    t: (key: ModelsKey) => string;
    /** Replace this row, preserving unrelated configuration. */
    onChange: (model: CodeSmithModelDraft) => void;
}
/**
 * Edit a nonempty set of input types, displaying inherited types before an override exists.
 * @param props - model declaration and row replacement action.
 * @returns the labeled text and image checkboxes.
 */
export declare function ModelInputTypes({ model, field, position, disabled, fallback, t, onChange }: ModelInputTypesProps): ReactNode;
export {};
//# sourceMappingURL=ModelInputTypes.d.ts.map