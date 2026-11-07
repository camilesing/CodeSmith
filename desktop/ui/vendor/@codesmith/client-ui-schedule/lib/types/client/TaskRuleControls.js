import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import clsx from 'clsx';
import { IconChevronDownOutlineRegular, IconChevronUpOutlineRegular, Pill } from '@codesmith/client-ui-primitives';
import css from './TaskManagerPage.module.css';
const WEEKDAYS = [1, 2, 3, 4, 5, 6, 7];
const WEEKDAY_LABELS = {
    1: 'frequency.weekday.1', 2: 'frequency.weekday.2', 3: 'frequency.weekday.3', 4: 'frequency.weekday.4',
    5: 'frequency.weekday.5', 6: 'frequency.weekday.6', 7: 'frequency.weekday.7',
};
/**
 * Render the weekly rule's Weekday row: one pill per weekday, named by the row's
 * label and toggled in the rule the caller is editing.
 * @param props.weekdays - weekdays the rule currently selects, as ISO weekday numbers (Monday is 1).
 * @param props.disabled - whether the rule's controls are read-only.
 * @param props.t - frequency translator owning the weekday labels.
 * @param props.rowId - id builder for the row's label, which names the pill group.
 * @param props.onToggle - toggle one weekday in the rule being edited.
 * @returns the labelled Weekday row.
 */
export function RuleWeekdayRow({ weekdays, disabled, t, rowId, onToggle }) {
    return _jsxs("div", { className: css.ruleRow, children: [_jsx("span", { className: css.ruleLabel, id: rowId('weekday'), children: t('rule.weekday') }), _jsx("div", { className: clsx(css.ruleWeekdays, css.ruleControl), role: "group", "aria-labelledby": rowId('weekday'), children: WEEKDAYS.map((weekday) => {
                    const selected = weekdays.includes(weekday);
                    return _jsx(Pill, { className: css.ruleControl, active: selected, disabled: disabled, "aria-pressed": selected, "aria-label": t('rule.weekdayOption', { weekday: t(WEEKDAY_LABELS[weekday]) }), onClick: () => { onToggle(weekday); }, children: t(WEEKDAY_LABELS[weekday]) }, weekday);
                }) })] });
}
/**
 * One whole-number stepper of the task rule form, such as the repeat interval or
 * the day of the month. Values clamp to the field's bounds, and an emptied or
 * fractional input stages nothing, so the staged rule stays valid on every edit.
 * @param props - bounds, staged value, blocking state, arrow labels, and callback.
 * @returns the stepper pill.
 */
export function RuleStepper({ id, min, max, value, disabled, increaseLabel, decreaseLabel, onChange }) {
    const clamp = (next) => Math.min(max, Math.max(min, next));
    return _jsxs("span", { className: css.ruleIntervalStepper, style: { '--interval-digits': String(value).length }, children: [_jsx("input", { id: id, className: clsx(css.ruleInput, css.ruleControl, css.ruleIntervalInput), type: "number", min: min, max: max, step: 1, disabled: disabled, value: value, onChange: (event) => {
                    const next = Number(event.target.value);
                    if (event.target.value === '' || !Number.isSafeInteger(next))
                        return;
                    onChange(clamp(next));
                } }), _jsxs("span", { className: css.ruleIntervalArrows, children: [_jsx("button", { type: "button", className: css.ruleIntervalArrow, "aria-label": increaseLabel, disabled: disabled || value >= max, onClick: () => { onChange(clamp(value + 1)); }, children: _jsx(IconChevronUpOutlineRegular, { size: 9 }) }), _jsx("button", { type: "button", className: css.ruleIntervalArrow, "aria-label": decreaseLabel, disabled: disabled || value <= min, onClick: () => { onChange(clamp(value - 1)); }, children: _jsx(IconChevronDownOutlineRegular, { size: 9 }) })] })] });
}
//# sourceMappingURL=TaskRuleControls.js.map