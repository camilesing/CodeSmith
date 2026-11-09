/** Local model-row identities, disclosures, capacity text, and draft ordering. */
import { useRef, useState } from 'react';
/** One editor-local identity; editing a model ID never replaces its controls. */
function createRow(key) {
    return { key, expanded: false, editing: new Map() };
}
/**
 * Keep row state with its model through draft moves and removals.
 * @param models - current draft array, owned by the provider card.
 * @param onChange - replaces only that card's draft array.
 * @param disabled - prevents reordering while the card cannot be edited.
 * @returns row state and local editing and ordering actions.
 */
export function useModelRows(models, onChange, disabled) {
    const nextKey = useRef(0);
    const [stored, setRows] = useState(() => models.map(() => createRow(nextKey.current++)));
    const [dragged, setDragged] = useState();
    const [target, setTarget] = useState();
    const entries = models.map((model, index) => ({ model, state: stored[index] ?? createRow(nextKey.current++) }));
    const rows = entries.map(entry => entry.state);
    if (stored.length !== models.length)
        setRows(rows);
    const endDrag = () => { setDragged(undefined); setTarget(undefined); };
    const move = (from, to) => {
        if (disabled || from === to || from < 0 || to < 0 || to >= models.length)
            return;
        const order = [...entries];
        const moved = order.splice(from, 1);
        order.splice(to, 0, ...moved);
        setRows(order.map(entry => entry.state));
        onChange(order.map(entry => entry.model));
    };
    return {
        rows: entries.map(({ model, state }, index) => ({
            ...state,
            model,
            order: {
                dragging: dragged === state.key,
                target: target?.key === state.key ? target.half : undefined,
                canDrop: !disabled && dragged !== undefined,
                onStart: () => { setDragged(state.key); },
                onEnd: endDrag,
                onOver: (half) => {
                    setTarget(current => current?.key === state.key && current.half === half ? current : { key: state.key, half });
                },
                onLeave: () => { setTarget(current => current?.key === state.key ? undefined : current); },
                onDrop: (half) => {
                    const from = rows.findIndex(row => row.key === dragged);
                    const insertion = index + (half === 'after' ? 1 : 0);
                    move(from, insertion > from ? insertion - 1 : insertion);
                    endDrag();
                },
                onMove: (offset) => { move(index, index + offset); },
            },
        })),
        toggle: (index) => {
            setRows(rows.map((row, at) => at === index ? { ...row, expanded: !row.expanded } : row));
        },
        edit: (index, field, text) => {
            setRows(rows.map((row, at) => {
                if (at !== index)
                    return row;
                const editing = new Map(row.editing);
                if (text === undefined)
                    editing.delete(field);
                else
                    editing.set(field, text);
                return { ...row, editing };
            }));
        },
        forget: (removed) => {
            setRows(rows.filter((_row, index) => !removed.has(index)));
            endDrag();
        },
        reset: () => { setRows(models.map(() => createRow(nextKey.current++))); endDrag(); },
    };
}
//# sourceMappingURL=useModelRows.js.map