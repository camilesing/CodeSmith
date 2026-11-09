import type { ToolCallViewProps } from '@codesmith/client-ui-tool/client';
import type { PropsLocale } from '@codesmith/client-ui-slots';
import type { NS } from './locales.ts';
type PresentRowProps = ToolCallViewProps & PropsLocale<typeof NS>;
/**
 * Render a present call using its recorded arguments and result.
 * @param props - tool call and localized status copy.
 * @returns a status row with a result disclosure.
 */
export declare function PresentRow(props: PresentRowProps): import("react").JSX.Element;
export {};
//# sourceMappingURL=PresentRow.d.ts.map