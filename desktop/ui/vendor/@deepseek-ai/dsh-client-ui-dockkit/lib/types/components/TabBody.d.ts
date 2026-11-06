import type { ReactNode } from 'react';
import type { TabRenderer } from '../contract/adapter.ts';
import type { TabRecord } from '../contract/types.ts';
/** One tab's body, re-rendered only when `renderTab` or `tab` is a new object. */
export declare const TabBody: import("react").MemoExoticComponent<({ tab, renderTab }: {
    readonly tab: TabRecord;
    readonly renderTab: TabRenderer;
}) => ReactNode>;
//# sourceMappingURL=TabBody.d.ts.map