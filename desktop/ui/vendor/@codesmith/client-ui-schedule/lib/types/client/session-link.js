/**
 * Check Host-list membership and archive state without activating or restoring anything.
 *
 * `SessionListState.byId` also carries local fallback rows for live Client
 * generations, so membership comes from `ids`, the Host-list projection: a
 * Session the Host list dropped reports unavailable even while a local row for
 * it survives.
 * @param id - Original Session bound to the task.
 * @param sessions - Current Session list projection.
 * @param workspaces - Current Workspace and archive projection.
 * @returns availability or the reason navigation is disabled.
 */
export function sessionLinkState(id, sessions, workspaces) {
    if (workspaces.state === 'error')
        return 'unavailable';
    if (sessions.phase === 'pending' || workspaces.phase === 'pending')
        return 'loading';
    if (workspaces.archivedSessionIds.includes(id))
        return 'archived';
    if (!sessions.ids.includes(id))
        return 'unavailable';
    return 'available';
}
//# sourceMappingURL=session-link.js.map