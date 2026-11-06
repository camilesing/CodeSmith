/** Platform-neutral assembly of generated Host Remote contributions. */
import productAnalyticsRemote from '@codesmith/client-product-analytics/remote';
import agentPresetsRemote from '@codesmith/agent-preset-registry/remote';
import userQuestionsRemote from '@codesmith/user-questions/remote';
import commandsRemote from '@codesmith/commands/remote';
import agentTeamsRemote from '@codesmith/agent-team/remote';
import accountRemote from '@codesmith/api-account-controller/remote';
import settingsControllerRemote from '@codesmith/api-settings-controller/remote';
import officeToPdfRemote from '@codesmith/office-to-pdf/remote';
import goalsRemote from '@codesmith/goal/remote';
import scheduleRemote from '@codesmith/schedule/remote';
import llmRemote from '@codesmith/llm/remote';
import dynamicRemote from '@codesmith/cordis-host-runner/remote';
import pluginManagerRemote from '@codesmith/plugin-manager/remote';
import mcpManagerRemote from '@codesmith/mcp-manager/remote';
import toolResultsRemote from '@codesmith/client-ui-trajectory/remote';
import pluginRegistryProbeRemote from '@codesmith/client-ui-plugin-manager/remote';
import pluginMarketRemote from '@codesmith/plugin-market/remote';
import pluginInventoryRemote from '@codesmith/host-plugin-inventory/remote';
import messageFeedbackRemote from '@codesmith/message-feedback/remote';
import permissionPresetsRemote from '@codesmith/permission-presets/remote';
import sessionFeedbackRemote from '@codesmith/command-feedback/remote';
import fileUploadsRemote from '@codesmith/client-file-upload/remote';
import sessionReferencesRemote from '@codesmith/session-reference/remote';
import subagentsRemote from '@codesmith/subagent/remote';
import sessionRemote from '@codesmith/api-session-controller/remote';
import jobRemote from '@codesmith/api-job-controller/remote';
import workspaceRemote from '@codesmith/api-workspace-controller/remote';
import terminalRemote from '@codesmith/api-terminal-controller/remote';
import workspaceFilesRemote from '@codesmith/api-workspace-files/remote';
import webRemote from '@codesmith/web/remote';
/** Required service: the typed Client Remote contribution mount. */
export const inject = ['remote'];
/**
 * Mount the Host capabilities explicitly selected for this Client assembly.
 * @param ctx - Client Cordis root carrying the typed API service.
 * @returns disposer after every selected Remote namespace is ready.
 */
export async function apply(ctx) {
    const disposers = [];
    try {
        for (const contribution of [
            productAnalyticsRemote, agentPresetsRemote, commandsRemote, agentTeamsRemote, settingsControllerRemote, accountRemote,
            goalsRemote, llmRemote, dynamicRemote, scheduleRemote,
            pluginInventoryRemote, pluginManagerRemote, mcpManagerRemote, pluginMarketRemote, pluginRegistryProbeRemote,
            toolResultsRemote,
            messageFeedbackRemote, sessionFeedbackRemote,
            fileUploadsRemote, sessionReferencesRemote,
            permissionPresetsRemote, subagentsRemote, sessionRemote, jobRemote, workspaceRemote, workspaceFilesRemote, terminalRemote,
            officeToPdfRemote, userQuestionsRemote, webRemote,
        ]) {
            disposers.push(await ctx.remote.$mount(contribution));
        }
    }
    catch (error) {
        for (const dispose of disposers.reverse())
            await dispose();
        throw error;
    }
    // Unwound in reverse mount order, so a namespace never outlives one mounted
    // after it.
    return async () => {
        for (const dispose of disposers.reverse())
            await dispose();
    };
}
//# sourceMappingURL=index.js.map