import type { Context } from '@codesmith/cordis';
import type { PropsLocale } from '@codesmith/client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
type WebRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/** Lets users expand a completed web search or fetch result. */
export declare function WebRow({ toolName, block, useContextMenu, sessionId, inspect, useDisclosure, t }: WebRowProps): import("react").JSX.Element;
/** Registers the web search and fetch conversation rows. */
export declare const webToolview: {
    name: string;
    inject: string[];
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=web-row.d.ts.map