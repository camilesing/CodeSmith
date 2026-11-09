import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
/**
 * A controlled file tree Component Factory over flat entries. The caller owns
 * the data, which directories are expanded, each directory's listing
 * state, and what a click does; the tree derives each entry's level from its
 * path, adds the directories the paths imply, orders every drawn level, and
 * draws the rows and their icons. It redraws when its
 * props or the occurrence's local selection change, and orders a drawn level
 * again only when the names or directory kinds in that level change.
 */
import { memo, useMemo } from 'react';
import clsx from 'clsx';
import { FileTypeIcon, classifyFileType, IconFolderCloseRegular, IconFolderOpenRegular } from '@codesmith/client-ui-primitives';
import css from './FileTree.module.css';
/** No row accessory unless the caller selects a local Component. */
function EmptyAccessory() {
    return null;
}
/** Natural, case-insensitive name order, so `file2` precedes `file10`. */
const byName = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
/**
 * The key of the level an entry is listed in.
 * @param path - the entry's key.
 * @param root - the root's key.
 * @returns the part of `path` before its last `/`, or `root` when it has none.
 */
function parentOf(path, root) {
    const slash = path.lastIndexOf('/');
    return slash < 0 ? root : path.slice(0, slash);
}
/**
 * Group entries under their parent keys, adding the directories their paths imply.
 * @param entries - the caller's entries; a repeated `path` keeps its first entry.
 * @param root - the root's key.
 * @returns each parent key's children.
 */
function levelsOf(entries, root) {
    const byPath = new Map();
    for (const entry of entries)
        if (entry.path !== root && !byPath.has(entry.path))
            byPath.set(entry.path, entry);
    for (const entry of entries) {
        if (entry.path === root)
            continue;
        for (let parent = parentOf(entry.path, root); parent !== root && parent !== '' && !byPath.has(parent); parent = parentOf(parent, root)) {
            byPath.set(parent, { path: parent, name: parent.slice(parent.lastIndexOf('/') + 1), meta: { kind: 'directory' } });
        }
    }
    const groups = new Map();
    for (const entry of byPath.values()) {
        const parent = parentOf(entry.path, root);
        const group = groups.get(parent);
        if (group === undefined)
            groups.set(parent, [entry]);
        else
            group.push(entry);
    }
    const levels = new Map();
    for (const [parent, group] of groups) {
        levels.set(parent, group.map(entry => ({ entry, kind: entry.meta?.kind ?? (groups.has(entry.path) ? 'directory' : 'file') })));
    }
    return levels;
}
/**
 * One level's display order: directories first, then everything else, each by name.
 * @param level - one parent key's children.
 * @returns the children's indices in display order.
 */
function orderOf(level) {
    return level.map((node, index) => ({ node, index })).sort(({ node: left }, { node: right }) => {
        const group = Number(right.kind === 'directory') - Number(left.kind === 'directory');
        return group !== 0 ? group : byName.compare(left.entry.name, right.entry.name);
    }).map(({ index }) => index);
}
/** One entry's row, and its children when it is an expanded directory. */
function Entry({ node, tree }) {
    const { input, Accessory } = tree;
    const { entry, kind } = node;
    const hidden = entry.meta?.hidden === true && css.hidden;
    const label = _jsx("span", { className: css.name, children: entry.name });
    if (kind === 'directory') {
        const expanded = tree.expanded.has(entry.path);
        return (_jsxs("li", { className: css.item, "data-files-entry": "directory", "data-files-path": entry.path, children: [_jsxs("div", { className: css.row, children: [_jsxs("button", { type: "button", className: clsx(css.action, hidden), "aria-expanded": expanded, onClick: () => { input.onToggle(entry, !expanded); }, ...input.rowTriggerProps?.(entry, 'directory'), children: [expanded ? _jsx(IconFolderOpenRegular, { className: css.icon }) : _jsx(IconFolderCloseRegular, { className: css.icon }), label] }), _jsx("div", { className: css.accessory, children: _jsx(Accessory, { entry: entry }) })] }), expanded && _jsx("ul", { className: css.level, children: _jsx(Level, { path: entry.path, tree: tree }) })] }));
    }
    if (kind === 'file' && input.canOpen?.(entry) !== false) {
        const current = entry.path === input.current;
        return (_jsx("li", { className: css.item, "data-files-entry": "file", "data-files-path": entry.path, children: _jsxs("div", { className: clsx(css.row, current && css.current), children: [_jsxs("button", { type: "button", className: clsx(css.action, hidden), "aria-current": current || undefined, ref: current ? input.currentRef : undefined, ...input.rowTriggerProps?.(entry, 'file'), onClick: () => { input.onOpen(entry); }, children: [_jsx(FileTypeIcon, { kind: classifyFileType(entry.name), size: 16, className: css.fileIcon }), label] }), _jsx("div", { className: css.accessory, children: _jsx(Accessory, { entry: entry }) })] }) }));
    }
    // A file the caller refuses is listed like anything else that does not open.
    const file = kind === 'file';
    return (_jsx("li", { className: css.item, "data-files-entry": kind, "data-files-path": entry.path, children: _jsxs("div", { className: clsx(css.row, css.inactive), children: [_jsxs("span", { className: clsx(css.action, hidden, css.other), "aria-disabled": "true", title: tree.t(file ? 'entry.noViewer' : 'entry.other'), children: [file && _jsx(FileTypeIcon, { kind: classifyFileType(entry.name), size: 16, className: css.fileIcon }), label] }), _jsx("div", { className: css.accessory, children: _jsx(Accessory, { entry: entry }) })] }) }));
}
/** No entries, shared so that an empty level orders nothing. */
const NO_ENTRIES = [];
/** One directory's rows: a loading line until it is listed, then its entries and notes. */
function Level({ path, tree }) {
    const { t } = tree;
    const state = tree.input.directories?.[path];
    const level = tree.levels.get(path) ?? NO_ENTRIES;
    // The order reads only each child's name and whether it is a directory, so
    // it is kept while those are unchanged; the rows draw this render's entries.
    const sortKey = JSON.stringify(level.map(node => [node.kind === 'directory', node.entry.name]));
    const order = useMemo(() => orderOf(level), [sortKey]);
    if (state?.loading === true)
        return _jsx("li", { className: css.note, "data-files-row": "loading", children: t('loading') });
    return (_jsxs(_Fragment, { children: [state?.failure !== undefined && _jsx("li", { className: css.note, "data-files-row": "failed", children: state.failure }), level.length === 0 && state?.failure === undefined && _jsx("li", { className: css.note, "data-files-row": "empty", children: t('empty') }), order.map(index => level[index]).filter(node => node !== undefined)
                .map(node => _jsx(Entry, { node: node, tree: tree }, node.entry.path)), state?.truncated === true && _jsx("li", { className: css.note, "data-files-row": "truncated", children: t('truncated') })] }));
}
/**
 * The tree from its root.
 * @param props - the caller's entries, expansion, states, callbacks, and framework seats.
 * @returns the root level's list.
 */
function FileTreeView({ t, useFactorySlot, ...input }) {
    const Accessory = useFactorySlot('accessory', EmptyAccessory);
    const levels = useMemo(() => levelsOf(input.entries, input.root), [input.entries, input.root]);
    const expanded = useMemo(() => new Set(input.expanded), [input.expanded]);
    const tree = { levels, expanded, input, t, Accessory };
    return _jsx("ul", { className: css.level, children: _jsx(Level, { path: input.root, tree: tree }) });
}
/**
 * The file tree. It redraws when a prop or the occurrence's local selection
 * changes, so a caller keeps its inputs and local selection stable between renders.
 */
export const FileTree = memo(FileTreeView);
//# sourceMappingURL=FileTree.js.map