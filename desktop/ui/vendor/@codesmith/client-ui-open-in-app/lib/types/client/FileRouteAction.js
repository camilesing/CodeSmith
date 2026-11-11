import { jsx as _jsx } from "react/jsx-runtime";
import { parseNativeFileApplications } from '@codesmith/native-command/types';
import { useFileApplications } from "./file-applications.js";
import { OpenTargetButton } from "./OpenTargetButton.js";
async function queryRoute(url, signal) {
    try {
        const response = await fetch(url, { signal });
        if (!response.ok)
            return null;
        return parseNativeFileApplications(await response.json());
    }
    catch (_error) {
        // Unavailable routes and malformed responses leave file reveal as the fallback.
        return null;
    }
}
/**
 * Render file actions without bypassing the owning Session's authorization route.
 * @param props - authenticated route, desktop availability, the native gesture
 * callback, and the announcement seat its settled outcome is reported to.
 * @returns the shared compact control, or null without a desktop.
 */
export function FileRouteAction(props) {
    const association = useFileApplications(props.actionUrl, queryRoute, props.available);
    if (!props.available)
        return null;
    return _jsx(OpenTargetButton, { kind: "file", applications: association.apps, defaultId: association.apps.find(app => app.default)?.id, failed: association.failed, loading: association.loading, busy: props.pending, refresh: association.refresh, t: props.t, report: props.report, execute: operation => props.onAction(operation.kind === 'reveal' ? 'reveal' : 'open', operation.kind === 'application' ? operation.id : undefined) }, props.actionUrl);
}
//# sourceMappingURL=FileRouteAction.js.map