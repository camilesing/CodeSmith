import { CredentialScanner } from "./credential-scanner.js";
import { formatTables } from "./format-tables.js";
/**
 * Create the credential matcher. Each instance owns a scanner whose memo holds
 * the last queried text until a query for another text.
 * @returns the `secret` rule; it reads no Facts, so Facts changes never invalidate its queries.
 */
export function createSecretRule() {
    const scanner = new CredentialScanner(formatTables());
    return {
        id: 'secret',
        // Same-start ties with other rules draw the credential masked.
        order: 0,
        match: (text, from) => scanner.match(text, from),
    };
}
//# sourceMappingURL=secret-rule.js.map