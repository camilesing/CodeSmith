/**
 * Compare captured JSON targets independently of object key insertion order.
 * @param target - captured immutable target.
 * @returns canonical JSON identity.
 */
export function menuTargetKey(target) {
    return JSON.stringify(target, (_key, value) => {
        if (typeof value !== 'object' || value === null || Array.isArray(value))
            return value;
        return Object.fromEntries(Object.entries(value).sort(([left], [right]) => left.localeCompare(right)));
    });
}
//# sourceMappingURL=target-key.js.map