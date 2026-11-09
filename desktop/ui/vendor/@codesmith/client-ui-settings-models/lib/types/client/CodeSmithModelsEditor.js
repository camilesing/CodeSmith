import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { AnimatedRows, IconPlusOutlineRegular } from '@codesmith/client-ui-primitives';
import { ModelRow } from "./ModelRow.js";
import { useModelRows } from "./useModelRows.js";
import styles from './ModelsSection.module.css';
/** Accepted capacity spellings: a decimal count with an optional K/M suffix. */
const CAPACITY_PATTERN = /^(\d+(?:\.\d+)?)([km])?$/i;
/** Decimal suffix scales — `1M` is 1000K, matching how model capacities are quoted. */
const CAPACITY_SCALE = { k: 1_000, m: 1_000_000 };
/**
 * Read a typed capacity, so a user can write `256K` or `1M` instead of counting
 * zeroes. The stored value stays a plain token count.
 * @param text - raw field text.
 * @returns the count; `undefined` when blank (inherit), `NaN` when unreadable
 * (rejected by {@link validateCodeSmithModels} before any write).
 */
export function parseCapacity(text) {
    const trimmed = text.trim();
    if (trimmed.length === 0)
        return undefined;
    const match = CAPACITY_PATTERN.exec(trimmed);
    if (match === null)
        return Number.NaN;
    const suffix = match[2]?.toLowerCase();
    const scale = suffix === 'k' || suffix === 'm' ? CAPACITY_SCALE[suffix] : 1;
    const scaled = Number(match[1]) * scale;
    // A decimal multiple is exact in intent but not in binary floating point
    // (2.3 * 1e6 lands a few ULPs high), so an integral intent snaps back.
    const rounded = Math.round(scaled);
    return Math.abs(scaled - rounded) < 1e-6 ? rounded : scaled;
}
/**
 * Spell a stored count back in the shortest form that survives a round trip
 * through {@link parseCapacity}; a count that is not a whole number of
 * thousands stays written out.
 * @param value - stored capacity.
 * @returns the field text.
 */
export function formatCapacity(value) {
    if (!Number.isInteger(value) || value <= 0)
        return String(value);
    if (value % CAPACITY_SCALE.m === 0)
        return `${String(value / CAPACITY_SCALE.m)}M`;
    if (value % CAPACITY_SCALE.k === 0)
        return `${String(value / CAPACITY_SCALE.k)}K`;
    return String(value);
}
/** Convert a schema-validated catalog value into records without dropping hidden fields. */
export function modelDrafts(value) {
    if (!Array.isArray(value))
        return [];
    return value.map(entry => typeof entry === 'object' && entry !== null && !Array.isArray(entry)
        ? entry
        : {});
}
/**
 * Validate adapter constraints that the serialized schema cannot express.
 * @param value - user-owned `models` value, or undefined while inherited.
 * @returns the first invalid row, or undefined when the adapter will accept it.
 */
export function validateCodeSmithModels(value) {
    if (value === undefined)
        return undefined;
    const models = modelDrafts(value);
    const seen = new Set();
    for (const [index, model] of models.entries()) {
        // Compared trimmed: surrounding whitespace is a paste artifact the adapter
        // would never match, and an untrimmed compare lets `model ` slip past the
        // duplicate check against its own twin.
        const id = model['id'];
        const trimmed = typeof id === 'string' ? id.trim() : undefined;
        if (trimmed === undefined || trimmed.length === 0)
            return { index, key: 'modelIdRequired' };
        if (seen.has(trimmed))
            return { index, key: 'modelIdDuplicate' };
        seen.add(trimmed);
        const name = model['name'];
        if (name !== undefined && (typeof name !== 'string' || name.length === 0)) {
            return { index, key: 'modelNameInvalid' };
        }
        const contextWindow = model['contextWindow'];
        if (contextWindow !== undefined
            && (typeof contextWindow !== 'number' || !Number.isInteger(contextWindow) || contextWindow <= 0)) {
            return { index, key: 'modelContextInvalid' };
        }
        const maxTokens = model['maxTokens'];
        if (maxTokens !== undefined
            && (typeof maxTokens !== 'number' || !Number.isInteger(maxTokens) || maxTokens <= 0)) {
            return { index, key: 'modelMaxTokensInvalid' };
        }
    }
    return undefined;
}
/**
 * Render the direct CodeSmith adapter's model catalog: id and display name on
 * each row, capacities and input types behind the row's own disclosure.
 * @param props - effective rows plus the array-level override actions.
 * @returns the catalog editor.
 */
export function CodeSmithModelsEditor(props) {
    const rowState = useModelRows(props.models, props.onChange, props.disabled);
    const update = (index, key, value) => {
        const next = props.models.map((model, at) => {
            const copy = { ...model };
            if (at !== index)
                return copy;
            if (value === undefined)
                Reflect.deleteProperty(copy, key);
            else
                copy[key] = value;
            return copy;
        });
        props.onChange(next);
    };
    const remove = (index) => {
        rowState.forget(new Set([index]));
        props.onChange(props.models.filter((_model, at) => at !== index).map(model => ({ ...model })));
    };
    const reset = () => {
        rowState.reset();
        props.onReset();
    };
    /** The field's text: its live keystrokes, else the stored count spelled short. */
    const capacityText = (model, index, field) => {
        const typed = rowState.rows[index]?.editing.get(field);
        if (typed !== undefined)
            return typed;
        const value = model[field];
        return typeof value === 'number' ? formatCapacity(value) : '';
    };
    const settleCapacity = (index, field) => {
        const typed = rowState.rows[index]?.editing.get(field);
        if (typed === undefined)
            return;
        // Unreadable text stays on screen: the save-time rejection names a row the
        // user can still see and correct.
        const parsed = parseCapacity(typed);
        if (parsed !== undefined && Number.isNaN(parsed))
            return;
        rowState.edit(index, field, undefined);
    };
    const capacityInput = (model, index, field, fallback) => ({
        value: capacityText(model, index, field),
        placeholder: fallback === undefined
            ? props.t(field === 'contextWindow' ? 'contextWindowPlaceholder' : 'maxTokensPlaceholder')
            : formatCapacity(fallback),
        onChange: (text) => {
            rowState.edit(index, field, text);
            update(index, field, parseCapacity(text));
        },
        onBlur: () => { settleCapacity(index, field); },
    });
    return (_jsxs("section", { className: styles['modelCatalog'], "aria-label": props.t('models'), children: [_jsxs("div", { className: styles['modelListHead'], children: [_jsxs("div", { className: styles['modelCatalogHeading'], children: [_jsx("span", { className: styles['modelCatalogTitle'], children: props.t('models') }), _jsx("span", { className: styles['modelCatalogMeta'], children: props.overridden ? props.t('modelsCustomized') : props.t('modelsInherited') })] }), props.overridden
                        ? (_jsx("button", { type: "button", className: styles['linkButton'], disabled: props.disabled, onClick: reset, children: props.t('resetModels') }))
                        : null] }), _jsxs(AnimatedRows, { className: styles['modelList'], rowKeys: rowState.rows.map(row => String(row.key)), ready: true, resetKey: "", children: [props.models.length === 0 ? _jsx("p", { className: styles['modelEmpty'], children: props.t('modelsEmpty') }) : null, rowState.rows.map(({ model, key, expanded, order }, index) => (_jsx(ModelRow, { rowKey: key, order: order, model: model, position: index + 1, inputField: "inputModalities", expanded: expanded, disabled: props.disabled, t: props.t, contextWindow: capacityInput(model, index, 'contextWindow', props.defaultContextWindow), maxTokens: capacityInput(model, index, 'maxTokens', props.defaultMaxTokens), onFieldChange: (field, value) => { update(index, field, value); }, onIdBlur: (value) => {
                            const trimmed = value.trim();
                            if (trimmed !== value)
                                update(index, 'id', trimmed);
                        }, onChange: (next) => { props.onChange(props.models.map((row, at) => at === index ? next : row)); }, onToggle: () => { rowState.toggle(index); }, onRemove: () => { remove(index); } }, key)))] }), _jsxs("button", { type: "button", className: styles['addModelButton'], disabled: props.disabled, onClick: () => { props.onChange([...props.models.map(model => ({ ...model })), { id: '' }]); }, children: [_jsx(IconPlusOutlineRegular, { size: 14 }), props.t('addModel')] })] }));
}
//# sourceMappingURL=CodeSmithModelsEditor.js.map