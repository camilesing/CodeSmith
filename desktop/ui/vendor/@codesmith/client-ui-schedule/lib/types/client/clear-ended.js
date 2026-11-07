/**
 * Create the management page's bulk removal callback.
 *
 * The call names no task ids: the Host owns which rows are ended, and a
 * delivery can have moved the catalog between the confirmation and the
 * request. The readback follows the acknowledged removal and supersedes a read
 * issued before it, so the list follows the removal rather than a stale read.
 * @param deps - Remote removal and its catalog readback.
 * @returns a callback that resolves once the settled outcome is available.
 */
export function createClearEnded(deps) {
    return async () => {
        let result;
        try {
            result = await deps.removeInactive();
        }
        catch (_error) {
            // A rejected request does not establish which rows changed.
            return { cleared: false };
        }
        if (!result.ok)
            return { cleared: false };
        await deps.refresh();
        return { cleared: true, count: result.value.deleted };
    };
}
//# sourceMappingURL=clear-ended.js.map