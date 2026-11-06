/** Original-text Session candidates with explicit wire spelling and longest known labels. */
import type { UserTextRule } from '@codesmith/client-ui-conversation/client';
declare module '@codesmith/client-ui-conversation/client' {
    interface UserTextDataMap {
        session: {
            readonly label: string;
        };
    }
    interface UserTextFacts {
        sessionLabels: readonly string[];
    }
}
/** Find the nearest Session reference; bare labels read their Facts only at a non-wire @ position. */
export declare const sessionReferenceRule: UserTextRule<'session'>;
//# sourceMappingURL=session-reference-rule.d.ts.map