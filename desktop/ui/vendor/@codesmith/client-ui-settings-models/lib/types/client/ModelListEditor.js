import { jsx as _jsx, jsxs as _jsxs, Fragment as _Fragment } from "react/jsx-runtime";
/**
 * The model list of one pi-ai provider profile, plus the action that asks the
 * provider what it serves.
 *
 * The list is the profile's `models` array as the card holds it: an empty list
 * means "serve this route's built-in catalog", and any entry replaces that
 * catalog, so a row is only ever added deliberately. Fetching asks the endpoint
 * **the form currently shows** — including a key typed but not yet saved — so
 * adding a provider is one pass instead of save-then-return; the reply is
 * candidates the user picks from, never configuration written behind them. The
 * picker opens with the configured models checked, so confirming it adds the
 * newly checked candidates and removes the unchecked configured ones in the
 * draft; configured rows the reply does not list are kept, and a confirmation
 * that would empty the list is refused.
 *
 * A provider that cannot be interrogated (an unreachable endpoint, a protocol
 * with no readable listing) is not a dead end: the failure is shown next to the
 * rows the user can still fill in by hand.
 */
import { useEffect, useMemo, useState } from 'react';
import { AnimatedRows, Button, IconPlusOutlineRegular, Modal } from '@codesmith/client-ui-primitives';
import { formatCapacity, parseCapacity } from "./CodeSmithModelsEditor.js";
import { ModelRow } from "./ModelRow.js";
import { useModelRows } from "./useModelRows.js";
import styles from './ModelsSection.module.css';
/** A row's text field, or the empty string when unset or not a string. */
function textOf(model, key) {
    const value = model[key];
    return typeof value === 'string' ? value : '';
}
/** A row's numeric field, or `undefined` when unset or not a number. */
function numberOf(model, key) {
    const value = model[key];
    return typeof value === 'number' ? value : undefined;
}
/**
 * What an empty capacity field is worth, shown as its placeholder so a row left
 * blank does not read as a model with no capacity at all.
 *
 * These hints match llm-pi-ai's built-in `defaultContextWindow` and
 * `defaultMaxTokens` in decimal K/M notation. Provider overrides are not
 * available to this component and are not reflected in the hints.
 */
const CAPACITY_HINT = {
    contextWindow: '1M',
    maxTokens: '128K',
};
/**
 * Spell a stored count for a field that may be unset. The spelling itself is
 * {@link formatCapacity}, shared with the CodeSmith catalog editor so both
 * surfaces read and write one K/M vocabulary.
 * @param value - stored capacity, or `undefined` for an unset field.
 * @returns the field text, empty when unset.
 */
function capacitySpelling(value) {
    return value === undefined ? '' : formatCapacity(value);
}
/** Adopt a candidate, preserving disclosed capacities and input types. */
function adopt(candidate) {
    return {
        id: candidate.id,
        ...candidate.name === undefined ? {} : { name: candidate.name },
        ...candidate.contextWindow === undefined ? {} : { contextWindow: candidate.contextWindow },
        ...candidate.maxTokens === undefined ? {} : { maxTokens: candidate.maxTokens },
        ...candidate.inputModalities === undefined ? {} : { input: [...candidate.inputModalities] },
    };
}
/**
 * Render the model list with its fetch action.
 * @param props - the drafted rows, probe target, wire face, and copy.
 * @returns the model-list editor.
 */
export function ModelListEditor(props) {
    const { models, onChange, probe, operations, t, disabled, onBusyChange } = props;
    const { catalogProvider } = props;
    const [busy, setBusy] = useState(false);
    useEffect(() => { onBusyChange(busy); }, [busy, onBusyChange]);
    const [failure, setFailure] = useState(undefined);
    const [inheritedCatalog, setInheritedCatalog] = useState(undefined);
    useEffect(() => {
        if (catalogProvider === undefined)
            return;
        let current = true;
        void operations.discoverModels(probe.settingsNs, { provider: catalogProvider }).then((answer) => {
            if (!current)
                return;
            setInheritedCatalog({ provider: catalogProvider, models: answer.kind === 'found' ? answer.models : [] });
            setFailure(answer.kind === 'refused' ? answer.message : undefined);
        });
        return () => { current = false; };
    }, [catalogProvider, operations, probe.settingsNs]);
    const catalog = inheritedCatalog?.provider === catalogProvider ? inheritedCatalog?.models : undefined;
    const inputDefaults = useMemo(() => new Map(catalog?.map(model => [model.id, model.inputModalities])), [catalog]);
    const [candidates, setCandidates] = useState(undefined);
    // Candidates whose checkbox the user flipped from its default. The default
    // is read from the list at render, so a row drafted while the fetch was in
    // flight opens checked rather than being removed by a stale selection.
    const [flipped, setFlipped] = useState(new Set());
    const [candidateQuery, setCandidateQuery] = useState('');
    const rowState = useModelRows(models, onChange, disabled);
    const editCapacity = (index, field, text) => {
        rowState.edit(index, field, text);
        patch(index, { [field]: parseCapacity(text) });
    };
    const capacityText = (model, index, field) => rowState.rows[index]?.editing.get(field) ?? capacitySpelling(numberOf(model, field));
    const patch = (index, next) => {
        onChange(models.map((model, at) => {
            if (at !== index)
                return model;
            // Rebuilt rather than spread over: an emptied optional field has to leave
            // the profile, not be stored as a value its schema would reject.
            // Spread first so a field this card does not edit survives; an emptied
            // optional field is then dropped rather than stored as a value its
            // schema would reject.
            const cleared = new Set(Object.entries(next).filter(([, value]) => value === undefined || value === '').map(([key]) => key));
            return Object.fromEntries(Object.entries({ ...model, ...next }).filter(([key]) => !cleared.has(key)));
        }));
    };
    const fetchModels = async () => {
        setBusy(true);
        setFailure(undefined);
        try {
            const answer = await operations.discoverModels(probe.settingsNs, {
                ...probe.provider === undefined ? {} : { provider: probe.provider },
                ...probe.baseURL === undefined || probe.baseURL.length === 0 ? {} : { baseURL: probe.baseURL },
                ...probe.api === undefined ? {} : { api: probe.api },
                ...probe.apiKey === undefined ? {} : { apiKey: probe.apiKey },
            });
            if (answer.kind === 'refused') {
                setFailure(answer.message);
                return;
            }
            const found = answer.models;
            if (catalogProvider !== undefined)
                setInheritedCatalog({ provider: catalogProvider, models: found });
            if (found.length === 0) {
                setFailure(t('fetchEmpty'));
                return;
            }
            setCandidateQuery('');
            setFlipped(new Set());
            // A listing may repeat an id; the picker and the list hold each model once.
            setCandidates([...new Map(found.map(model => [model.id, model])).values()]);
        }
        finally {
            setBusy(false);
        }
    };
    const closePicker = () => {
        setCandidates(undefined);
        setFlipped(new Set());
        setCandidateQuery('');
    };
    // The picker opens on the current list so a few models can be added or
    // removed in one place. A list with no identified model serves the whole
    // catalog or nothing yet, so every candidate starts checked.
    const configuredIds = new Set(models.map(model => textOf(model, 'id')).filter(id => id.length > 0));
    const pickedByDefault = (id) => configuredIds.size === 0 || configuredIds.has(id);
    const isPicked = (id) => pickedByDefault(id) !== flipped.has(id);
    const activeCandidates = candidates ?? [];
    const listed = new Set(activeCandidates.map(candidate => candidate.id));
    // An unchecked candidate leaves the list. A row the listing does not name,
    // including a half-typed row without an id, is outside the picker and stays.
    const removedRows = new Set();
    models.forEach((model, at) => {
        const id = textOf(model, 'id');
        if (listed.has(id) && !isPicked(id))
            removedRows.add(at);
    });
    // A kept row the user already tuned wins over the provider's own numbers.
    const keptRows = models.filter((_model, at) => !removedRows.has(at));
    const keptIds = new Set(keptRows.map(model => textOf(model, 'id')));
    const addedRows = activeCandidates
        .filter(candidate => isPicked(candidate.id) && !keptIds.has(candidate.id))
        .map(adopt);
    // An empty list serves the whole catalog, so emptying a non-empty list
    // would restore every model instead of removing the unchecked ones.
    const emptiesList = models.length > 0 && keptRows.length === 0 && addedRows.length === 0;
    const adoptPicked = () => {
        onChange([...keptRows, ...addedRows]);
        rowState.forget(removedRows);
        closePicker();
    };
    const toggle = (id) => {
        setFlipped((current) => {
            const next = new Set(current);
            if (!next.delete(id))
                next.add(id);
            return next;
        });
    };
    const normalizedCandidateQuery = candidateQuery.trim().toLowerCase();
    const visibleCandidates = normalizedCandidateQuery.length === 0
        ? activeCandidates
        : activeCandidates.filter(candidate => candidate.id.toLowerCase().includes(normalizedCandidateQuery)
            || candidate.name?.toLowerCase().includes(normalizedCandidateQuery) === true);
    const allVisibleCandidatesPicked = visibleCandidates.length > 0
        && visibleCandidates.every(candidate => isPicked(candidate.id));
    const toggleVisibleCandidates = () => {
        // Only visible candidates change: a hidden checked one may be a configured
        // model, and clearing it would remove that model unseen.
        const target = !allVisibleCandidatesPicked;
        setFlipped((current) => {
            const next = new Set(current);
            for (const candidate of visibleCandidates) {
                if (pickedByDefault(candidate.id) === target)
                    next.delete(candidate.id);
                else
                    next.add(candidate.id);
            }
            return next;
        });
    };
    // A route the adapter already describes answers without an endpoint; only a
    // draft with neither has nothing to ask about.
    const askable = probe.provider !== undefined || (probe.baseURL !== undefined && probe.baseURL.length > 0);
    return (_jsxs("section", { className: styles['modelCatalog'], "aria-label": t('models'), children: [_jsxs("div", { className: styles['modelListHead'], children: [_jsxs("div", { className: styles['modelCatalogHeading'], children: [_jsx("span", { className: styles['modelCatalogTitle'], children: t('models') }), props.overridden === undefined
                                ? null
                                : (_jsx("span", { className: styles['modelCatalogMeta'], children: props.overridden ? t('modelsCustomized') : t('modelsInherited') }))] }), props.overridden === true && props.onReset !== undefined
                        ? (_jsx("button", { type: "button", className: styles['linkButton'], disabled: disabled, onClick: () => { rowState.reset(); props.onReset?.(); }, children: t('resetModels') }))
                        : null, _jsx("button", { type: "button", className: styles['linkButton'], disabled: disabled || busy || !askable || props.probeBlocked !== undefined, title: props.probeBlocked !== undefined
                            ? t(props.probeBlocked)
                            : askable ? undefined : t('fetchNeedsBaseUrl'), onClick: () => { void fetchModels(); }, children: busy ? t('fetching') : t('fetchModels') })] }), models.length === 0 ? _jsx("p", { className: styles['modelEmpty'], children: t('modelsEmpty') }) : null, _jsx(AnimatedRows, { className: styles['modelList'], rowKeys: rowState.rows.map(row => String(row.key)), ready: true, resetKey: "", children: rowState.rows.map(({ model, key, expanded, order }, index) => (_jsx(ModelRow, { rowKey: key, order: order, model: model, position: index + 1, inputField: "input", inputFallback: inputDefaults.get(textOf(model, 'id')) ?? props.defaultInput, inputLoading: catalogProvider !== undefined && catalog === undefined, reasoningLevels: props.reasoningLevels, expanded: expanded, disabled: disabled, t: t, contextWindow: {
                        value: capacityText(model, index, 'contextWindow'),
                        placeholder: CAPACITY_HINT.contextWindow,
                        onChange: (text) => { editCapacity(index, 'contextWindow', text); },
                    }, maxTokens: {
                        value: capacityText(model, index, 'maxTokens'),
                        placeholder: CAPACITY_HINT.maxTokens,
                        onChange: (text) => { editCapacity(index, 'maxTokens', text); },
                    }, onFieldChange: (field, value) => { patch(index, { [field]: value }); }, onChange: (next) => { onChange(models.map((row, at) => at === index ? next : row)); }, onToggle: () => { rowState.toggle(index); }, onRemove: () => {
                        onChange(models.filter((_model, at) => at !== index));
                        rowState.forget(new Set([index]));
                    } }, key))) }), _jsxs("button", { type: "button", className: styles['addModelButton'], disabled: disabled, onClick: () => { onChange([...models, { id: '' }]); }, children: [_jsx(IconPlusOutlineRegular, { size: 14 }), t('addModel')] }), failure !== undefined ? _jsx("p", { className: styles['error'], children: failure }) : null, _jsxs(Modal, { open: candidates !== undefined, onClose: closePicker, title: t('fetchTitle'), closeLabel: t('close'), description: t('fetchDescription'), className: styles['fetchDialog'], footer: (_jsxs(_Fragment, { children: [_jsx(Button, { variant: "outline", onClick: closePicker, children: t('cancel') }), _jsx(Button, { variant: "outline", disabled: emptiesList, title: emptiesList ? t('fetchKeepOne') : undefined, onClick: adoptPicked, children: t('fetchAdopt') })] })), children: [_jsxs("div", { className: styles['candidateToolbar'], children: [_jsx("input", { className: `${styles['input']} ${styles['candidateSearch']}`, type: "search", value: candidateQuery, placeholder: t('fetchSearch'), "aria-label": t('fetchSearch'), onChange: (event) => { setCandidateQuery(event.target.value); } }), _jsx(Button, { variant: "ghost", size: "sm", disabled: visibleCandidates.length === 0, onClick: toggleVisibleCandidates, children: t(allVisibleCandidatesPicked ? 'fetchDeselectAll' : 'fetchSelectAll') })] }), visibleCandidates.length === 0
                        ? _jsx("p", { className: styles['candidateEmpty'], role: "status", children: t('fetchNoMatches') })
                        : (_jsx("ul", { className: styles['candidateList'], children: visibleCandidates.map(candidate => (_jsx("li", { className: styles['candidate'], children: _jsxs("label", { className: styles['candidateLabel'], children: [_jsx("input", { type: "checkbox", checked: isPicked(candidate.id), onChange: () => { toggle(candidate.id); } }), _jsx("span", { className: styles['candidateId'], title: candidate.name ?? candidate.id, children: candidate.id })] }) }, candidate.id))) }))] })] }));
}
//# sourceMappingURL=ModelListEditor.js.map