import { childPath } from "./store.js";
/**
 * Say why a directory could not be listed, in terms of the directory.
 * @param t - namespace-bound translate.
 * @param failure - the settled Remote failure.
 * @returns the line to show under the directory.
 */
export function failureLine(t, failure) {
    switch (failure.code) {
        case 'workspace-file/not-found': return t('error.notFound');
        case 'workspace-file/outside-workspace': return t('error.outsideWorkspace');
        case 'workspace-file/not-directory': return t('error.notDirectory');
        // Carrier and unclassified host failures reach the reader as themselves:
        // this tree knows nothing useful to add to a transport-level message.
        default: return t('error.unavailable', { message: failure.message });
    }
}
/**
 * The tree key of a workspace root.
 * @param root - the root's absolute path as the Host reports it.
 * @returns the path without trailing separators.
 */
export function treeRootOf(root) {
    return root.replace(/[/\\]+$/, '');
}
/**
 * The store key of the directory a tree key's entry is listed in.
 * @param tree - the pane's tree.
 * @param path - an entry's tree key.
 * @returns the absolute path of its parent, the root spelled as the store keeps it.
 */
export function parentOf(tree, path) {
    const parent = path.slice(0, path.lastIndexOf('/'));
    return parent === treeRootOf(tree.root) ? tree.root : parent;
}
/**
 * Every listed entry of a pane's tree, collapsed levels included, each named
 * as its listing spells it.
 * @param levels - the pane's level states.
 * @returns the entries keyed as the tree draws them.
 */
export function workspaceEntries(levels) {
    return Object.entries(levels).flatMap(([path, level]) => level.kind !== 'ready' ? [] : level.level.entries
        .map(entry => ({ path: childPath(path, entry.name), name: entry.name, meta: { kind: entry.type } })));
}
/**
 * Each directory's listing state: loading until its first listing settles, then
 * its latest failure and whether its listing is truncated.
 * @param tree - the pane's root, level states, and expanded directories.
 * @param t - namespace-bound translate, for failure lines.
 * @returns the states keyed as the tree draws them.
 */
export function workspaceDirectories(tree, t) {
    const { root, levels, expanded } = tree;
    const directories = {};
    const keyOf = (path) => path === root ? treeRootOf(path) : path;
    for (const path of expanded)
        directories[keyOf(path)] = { loading: true };
    for (const [path, level] of Object.entries(levels)) {
        const key = keyOf(path);
        if (level.kind === 'loading')
            directories[key] = { loading: true };
        else if (level.kind === 'failed')
            directories[key] = { failure: failureLine(t, level.failure) };
        else {
            const state = {};
            if (level.failure !== undefined)
                state.failure = failureLine(t, level.failure);
            if (level.level.truncated)
                state.truncated = true;
            directories[key] = state;
        }
    }
    return directories;
}
//# sourceMappingURL=workspace-tree.js.map