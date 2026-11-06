import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
/** Running Turn clock isolated from the transcript's render cycle. */
import { memo, useEffect, useState } from 'react';
import { TextShimmer } from '@codesmith/client-ui-primitives';
import { formatRunDuration, LIVE_RUN_CLOCK_INTERVAL_MS } from "./message-chrome.js";
import { RunningWhaleTail } from "./RunningWhaleTail.js";
import a11yCss from './accessibility.module.css';
import css from './ChatView.module.css';
/**
 * Show live elapsed time after the current Turn's content without announcing ticks.
 * @param props - Current Turn start time, pause state, and localized copy.
 * @returns the blue running indicator; mount while the Session is running, or holding its slot hidden until the
 * Turn's actions row replaces it.
 */
export const RunningStatus = memo(function RunningStatus({ startTime, paused, holding = false, t }) {
    const [now, setNow] = useState(Date.now);
    useEffect(() => {
        if (startTime === undefined || paused || holding)
            return;
        setNow(Date.now());
        const timer = setInterval(() => { setNow(Date.now()); }, LIVE_RUN_CLOCK_INTERVAL_MS);
        return () => { clearInterval(timer); };
    }, [startTime, paused, holding]);
    const label = startTime === undefined ? t('chat.deepDiving') : t('chat.deepDivingFor', {
        duration: formatRunDuration(Math.max(1000, now - startTime), t).map(part => part.text).join(''),
    });
    return (_jsxs("div", { className: css.running, "data-chat-running": true, "data-holding": holding ? '' : undefined, "aria-hidden": holding || undefined, children: [holding ? null : (_jsx("span", { className: a11yCss.visuallyHidden, role: "status", "aria-live": "polite", "aria-atomic": "true", children: t('chat.deepDiving') })), _jsx("span", { className: css.runningDivider, "aria-hidden": "true" }), _jsxs("span", { className: css.runningContent, children: [_jsx(RunningWhaleTail, {}), _jsx(TextShimmer, { active: !holding, className: css.runningText, children: label })] })] }));
});
//# sourceMappingURL=RunningStatus.js.map