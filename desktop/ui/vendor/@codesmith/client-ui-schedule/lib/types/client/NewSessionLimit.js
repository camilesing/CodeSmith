import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Display the Host's effective admission interval before selecting per-run conversations. */
import { useEffect, useState } from 'react';
import { Button, StateDot } from '@codesmith/client-ui-primitives';
import css from './TaskManagerPage.module.css';
/** Read the configured interval through an injected action; failed reads stay visible and retryable.
 * @param props - The failure-contained read action and task-manager translations.
 * @returns The configured interval, a loading indicator, or a retryable failure.
 */
export function NewSessionLimit({ load, t }) {
    const [attempt, setAttempt] = useState(0);
    const [interval, setInterval] = useState('loading');
    useEffect(() => {
        let active = true;
        setInterval('loading');
        void load().then((seconds) => {
            if (active)
                setInterval(seconds ?? 'error');
        });
        return () => { active = false; };
    }, [load, attempt]);
    if (interval === 'loading')
        return _jsx("div", { className: css.saveNotice, role: "status", "aria-label": t('host.limitsLoading'), children: _jsx(StateDot, { state: "ongoing" }) });
    if (interval === 'error')
        return _jsxs("div", { className: css.notice, children: [_jsx("p", { role: "alert", children: t('host.limitsError') }), _jsx(Button, { size: "sm", onClick: () => { setAttempt(value => value + 1); }, children: t('list.retry') })] });
    const unit = interval % 3_600 === 0 ? 'hour' : interval % 60 === 0 ? 'minute' : 'second';
    const value = interval / (unit === 'hour' ? 3_600 : unit === 'minute' ? 60 : 1);
    const label = new Intl.NumberFormat(t('time.locale'), { style: 'unit', unit, unitDisplay: 'long' }).format(value);
    return _jsx("p", { className: css.saveNotice, children: t('host.newSessionInterval', { interval: label }) });
}
//# sourceMappingURL=NewSessionLimit.js.map