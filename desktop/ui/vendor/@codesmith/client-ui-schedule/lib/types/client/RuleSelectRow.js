import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** The existing task rule menu treatment shared by target and timing fields. */
import { useId, useMemo, useState } from 'react';
import clsx from 'clsx';
import { IconChevronDownOutlineRegular } from '@codesmith/client-ui-primitives';
import { TaskMenu } from "./TaskMenu.js";
import { zoneChoices, zoneName } from "./schedule-format.js";
import { loadRecentTimeZones, rememberTimeZone } from "./recent-time-zones.js";
import css from './TaskManagerPage.module.css';
/** A labelled menu row using the current task detail's trigger, portal, and keyboard behavior. */
export function RuleSelectRow({ label, value, current, items, disabled, onChange, onDismiss, header, listClassName }) {
    const [open, setOpen] = useState(false);
    const close = () => { setOpen(false); onDismiss?.(); };
    return _jsx(TaskMenu, { className: css.ruleRowMenu, open: open && !disabled, onClose: close, items: items, selectedId: value, align: "end", portal: true, header: header, listClassName: listClassName, onSelect: (id) => { close(); onChange(id); }, anchor: _jsxs("button", { type: "button", className: css.ruleValue, disabled: disabled, "aria-haspopup": "menu", "aria-expanded": open && !disabled, onClick: () => { if (open)
                close();
            else
                setOpen(true); }, children: [_jsx("span", { className: css.ruleLabel, children: label }), ' ', _jsxs("span", { className: clsx(css.ruleValueFace, css.ruleControl), children: [_jsx("span", { className: css.ruleCurrent, children: current }), _jsx(IconChevronDownOutlineRegular, {})] })] }) });
}
/** The current task detail's searchable, localized zone menu with recent choices. */
export function RuleZoneRow({ value, disabled, onChange, t }) {
    const systemZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    const [query, setQuery] = useState('');
    const [recent, setRecent] = useState(() => loadRecentTimeZones(systemZone));
    const id = useId();
    const catalog = useMemo(() => {
        const byLabel = new Map();
        const at = Date.now();
        for (const zone of zoneChoices(value, systemZone, at)) {
            const label = zoneName(zone, systemZone, t, at);
            const existing = byLabel.get(label);
            if (existing === undefined)
                byLabel.set(label, { id: zone, label, zones: [zone] });
            else {
                existing.zones.push(zone);
                if (zone === value)
                    existing.id = zone;
            }
        }
        return [...byLabel.values()];
    }, [value, systemZone, t]);
    const normalized = query.trim().toLocaleLowerCase(t('time.locale'));
    const matches = catalog.filter(item => `${item.zones.join(' ')} ${item.label}`.toLocaleLowerCase(t('time.locale')).includes(normalized))
        .map(item => ({ ...item, id: item.zones.find(zone => zone.toLocaleLowerCase(t('time.locale')) === normalized) ?? item.id }));
    const recentItems = normalized === '' ? recent.flatMap((zone) => {
        const item = catalog.find(item => item.zones.includes(zone));
        return item === undefined ? [] : [{ ...item, id: zone }];
    }).filter((item, index, items) => items.findIndex(candidate => candidate.label === item.label) === index) : [];
    const available = matches.filter(item => !recentItems.some(recent => recent.label === item.label));
    const items = recentItems.length === 0 ? available : available.length === 0 ? recentItems
        : [...recentItems, { type: 'separator', id: `${id}-recent` }, ...available];
    return _jsx(RuleSelectRow, { label: t('timing.zone'), value: value, current: zoneName(value, systemZone, t), disabled: disabled, onDismiss: () => { setQuery(''); }, items: items.length === 0 ? [{ id: `${id}-empty`, label: t('timing.zoneNoResults'), disabled: true }] : items, listClassName: css.zoneMenu, header: _jsx("input", { className: css.zoneSearch, type: "search", value: query, "data-menu-field": "", placeholder: t('timing.zoneSearch'), "aria-label": t('timing.zoneSearch'), onChange: (event) => { setQuery(event.target.value); } }), onChange: (zone) => { setQuery(''); setRecent(recent => rememberTimeZone(recent, zone)); onChange(zone); } });
}
//# sourceMappingURL=RuleSelectRow.js.map