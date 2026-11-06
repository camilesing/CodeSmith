/** Platform-neutral assembly of generated Host Remote contributions. */
import type { Context } from '@codesmith/cordis';
export type {} from '@codesmith/client-product-analytics/remote';
export type {} from '@codesmith/agent-team/remote';
import type { ClientRemote } from '@codesmith/api-gateway/client';
export type { ClientRemote } from '@codesmith/api-gateway/client';
export type { BundleDetail, BundleInfo, BundleRowInfo, ChangeResult, IncompatiblePlugin, InspectOptions, InstallBundleOptions, InstallSpecKind, ManagementError, PackageResult, PluginChange, PluginEntryId, PluginInfo, PluginInspectProblem, PluginInstallCancellation, PluginInstallFailureKind, PluginInstallLogChunk, PluginInstallProgress, PluginInstallRequestId, PluginRegistries, PluginSpecInspection, PluginUpdateProgress, PluginUpdateResult, PluginUpdateStep, PluginVersion, PluginVersionList, RegistryAttempt, ReadOnlyReason, Registry, RestartReason, } from '@codesmith/plugin-manager/types';
export type { McpManagedServerStatus, McpManagedServerView } from '@codesmith/mcp-manager/types';
export type {} from '@codesmith/plugin-manager/remote';
export type {} from '@codesmith/mcp-manager/remote';
export type {} from '@codesmith/client-ui-plugin-manager/remote';
export type {} from '@codesmith/client-ui-trajectory/remote';
export type {} from '@codesmith/plugin-market/remote';
export type { MarketCompatibility, MarketIndexSummary, MarketInstallation, MarketInstallationStatus, MarketListingEntry, MarketListingVersion, MarketQuery, MarketSearchResult, MarketSort, PluginDetail, PluginDetailDocumentId, PluginDetailOrigin, } from '@codesmith/plugin-market/types';
export type { PluginInventorySnapshot } from '@codesmith/host-plugin-inventory/types';
export type {} from '@codesmith/agent-preset-registry/remote';
export type {} from '@codesmith/user-questions/remote';
export type {} from '@codesmith/commands/remote';
export type {} from '@codesmith/web/remote';
export type {} from '@codesmith/api-settings-controller/remote';
export type {} from '@codesmith/api-account-controller/remote';
export type {} from '@codesmith/goal/remote';
export type {} from '@codesmith/schedule/remote';
export type {} from '@codesmith/office-to-pdf/remote';
export type {} from '@codesmith/llm/remote';
export type {} from '@codesmith/host-plugin-inventory/remote';
export type {} from '@codesmith/message-feedback/remote';
export type {} from '@codesmith/permission-presets/remote';
export type {} from '@codesmith/command-feedback/remote';
export type {} from '@codesmith/client-file-upload/remote';
export type {} from '@codesmith/session-reference/remote';
export type {} from '@codesmith/subagent/remote';
export type * from '@codesmith/subagent/client';
export type {} from '@codesmith/api-session-controller/remote';
export type * from '@codesmith/api-session-controller/types';
export type {} from '@codesmith/api-job-controller/remote';
export type * from '@codesmith/api-job-controller/types';
export type {} from '@codesmith/api-workspace-controller/remote';
export type * from '@codesmith/api-workspace-controller/types';
export type {} from '@codesmith/api-workspace-files/remote';
export type * from '@codesmith/api-workspace-files/types';
export type {} from '@codesmith/api-terminal-controller/remote';
export type * from '@codesmith/api-terminal-controller/types';
export type { ApiRemoteForwardedEvent } from '../types.ts';
export type {} from '@codesmith/commands/types';
export type {} from '@codesmith/web/types';
export type {} from '@codesmith/cordis-host-runner/types';
export type {} from '@codesmith/credentials/types';
export type {} from '@codesmith/llm/types';
export type {} from '@codesmith/agent-preset-registry/types';
export type {} from '@codesmith/permission-presets/types';
export type {} from '@codesmith/settings/types';
export type {} from '@codesmith/user-approval/types';
export type {} from '@codesmith/user-questions/types';
export type {} from '@codesmith/api-session-controller/types';
/**
 * The carrier's Client-facing types, re-exported so a business package names one
 * assembly package instead of both this facade and the Connection plugin. Type-only:
 * the carrier's runtime values stay behind their own module edge.
 */
export type { ConnectionHandle, ConnectionSinks, ContentBlock, MessageId, RpcId, RpcRequest, RpcResponse, RpcResult, SessionId, StreamChunk, } from '@codesmith/client-connection/client';
export type {} from '@codesmith/api-gateway/client';
export type {} from '@codesmith/cordis-host-runner/remote';
export type { ApprovalRequestId, CordisHalfState, CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId, CordisDynamicRunMode, CordisInspectMethodManifest, CordisInspectPlatform, CordisInspectProviderManifest, CordisInspectProviderView, CordisInspectQueryRequest, CordisInspectQueryResolution, CordisInspectQueryResolved, CordisInspectRequestId, CordisInspectResolveAck, CordisRunDiagnostic, CordisRunStatus, DynamicCordisClientSource, DynamicCordisHostHalfResult, DynamicCordisInventoryRow, DynamicCordisInvokeResult, DynamicCordisPackage, DynamicCordisRequestResolved, DynamicCordisResolveAck, DynamicCordisRetracted, DynamicCordisRunRequest, DynamicCordisRunResolution, DynamicCordisRunAttempt, DynamicCordisRunResponse, DynamicCordisStopResponse, DynamicCordisUndefineReceipt, RequestRunOutcome, } from '@codesmith/cordis-host-runner/types';
export type { CredentialInfo } from '@codesmith/credentials/types';
export type { SettingsDescribeValue, SettingsNamespaceView, SettingsPathOpView, SettingsSecretView, } from '@codesmith/settings/types';
export type { LlmConfigurableProvider, LlmDiscoveredModel, LlmModelDiscoveryRequest, LlmNativeAuth, LlmProviderInfo, } from '@codesmith/llm/types';
export type { FileReferenceCandidate } from '@codesmith/file-reference/types';
export type { SessionReferenceMentionCandidate } from '@codesmith/session-reference/types';
export type { RemoteErrorCode, RemoteErrorDetailsMap, RemoteFailure, RemoteResult, } from '@codesmith/typert-protocol';
export type { RemoteHostFacts } from '@codesmith/api-gateway/client';
declare module '@codesmith/cordis' {
    interface Context {
        /** Generated Remote namespaces selected by this Client assembly. */
        remote: ClientRemote;
    }
}
/** Required service: the typed Client Remote contribution mount. */
export declare const inject: string[];
/**
 * Mount the Host capabilities explicitly selected for this Client assembly.
 * @param ctx - Client Cordis root carrying the typed API service.
 * @returns disposer after every selected Remote namespace is ready.
 */
export declare function apply(ctx: Context): Promise<() => Promise<void>>;
//# sourceMappingURL=index.d.ts.map