/** Platform-neutral assembly of generated Host Remote contributions. */

import type { Context } from '@codesmith/cordis'
import productAnalyticsRemote from '@codesmith/client-product-analytics/remote'
export type {} from '@codesmith/client-product-analytics/remote'
import agentPresetsRemote from '@codesmith/agent-preset-registry/remote'
import userQuestionsRemote from '@codesmith/user-questions/remote'
import commandsRemote from '@codesmith/commands/remote'
import agentTeamsRemote from '@codesmith/agent-team/remote'
export type {} from '@codesmith/agent-team/remote'
import accountRemote from '@codesmith/api-account-controller/remote'
import settingsControllerRemote from '@codesmith/api-settings-controller/remote'
import officeToPdfRemote from '@codesmith/office-to-pdf/remote'
import goalsRemote from '@codesmith/goal/remote'
import scheduleRemote from '@codesmith/schedule/remote'
import llmRemote from '@codesmith/llm/remote'
import dynamicRemote from '@codesmith/cordis-host-runner/remote'
import pluginManagerRemote from '@codesmith/plugin-manager/remote'
import mcpManagerRemote from '@codesmith/mcp-manager/remote'
import toolResultsRemote from '@codesmith/client-ui-trajectory/remote'
import pluginRegistryProbeRemote from '@codesmith/client-ui-plugin-manager/remote'
import pluginMarketRemote from '@codesmith/plugin-market/remote'
import pluginInventoryRemote from '@codesmith/host-plugin-inventory/remote'
import messageFeedbackRemote from '@codesmith/message-feedback/remote'
import permissionPresetsRemote from '@codesmith/permission-presets/remote'
import sessionFeedbackRemote from '@codesmith/command-feedback/remote'
import fileUploadsRemote from '@codesmith/client-file-upload/remote'
import sessionReferencesRemote from '@codesmith/session-reference/remote'
import subagentsRemote from '@codesmith/subagent/remote'
import sessionRemote from '@codesmith/api-session-controller/remote'
import jobRemote from '@codesmith/api-job-controller/remote'
import workspaceRemote from '@codesmith/api-workspace-controller/remote'
import terminalRemote from '@codesmith/api-terminal-controller/remote'
import workspaceFilesRemote from '@codesmith/api-workspace-files/remote'
import webRemote from '@codesmith/web/remote'
import type { ClientRemote } from '@codesmith/api-gateway/client'

export type { ClientRemote } from '@codesmith/api-gateway/client'
export type {
  BundleDetail, BundleInfo, BundleRowInfo, ChangeResult, IncompatiblePlugin, InspectOptions, InstallBundleOptions, InstallSpecKind,
  ManagementError, PackageResult,
  PluginChange, PluginEntryId, PluginInfo, PluginInspectProblem, PluginInstallCancellation, PluginInstallFailureKind,
  PluginInstallLogChunk, PluginInstallProgress, PluginInstallRequestId, PluginRegistries, PluginSpecInspection,
  PluginUpdateProgress, PluginUpdateResult, PluginUpdateStep, PluginVersion, PluginVersionList, RegistryAttempt,
  ReadOnlyReason, Registry, RestartReason,
} from '@codesmith/plugin-manager/types'
export type { McpManagedServerStatus, McpManagedServerView } from '@codesmith/mcp-manager/types'
export type {} from '@codesmith/plugin-manager/remote'
export type {} from '@codesmith/mcp-manager/remote'
export type {} from '@codesmith/client-ui-plugin-manager/remote'
export type {} from '@codesmith/client-ui-trajectory/remote'
export type {} from '@codesmith/plugin-market/remote'
export type {
  MarketCompatibility, MarketIndexSummary, MarketInstallation, MarketInstallationStatus, MarketListingEntry, MarketListingVersion,
  MarketQuery, MarketSearchResult, MarketSort, PluginDetail, PluginDetailDocumentId, PluginDetailOrigin,
} from '@codesmith/plugin-market/types'
export type { PluginInventorySnapshot } from '@codesmith/host-plugin-inventory/types'
export type {} from '@codesmith/agent-preset-registry/remote'
export type {} from '@codesmith/user-questions/remote'
export type {} from '@codesmith/commands/remote'
export type {} from '@codesmith/web/remote'
export type {} from '@codesmith/api-settings-controller/remote'
export type {} from '@codesmith/api-account-controller/remote'
export type {} from '@codesmith/goal/remote'
export type {} from '@codesmith/schedule/remote'
export type {} from '@codesmith/office-to-pdf/remote'
export type {} from '@codesmith/llm/remote'
export type {} from '@codesmith/host-plugin-inventory/remote'
export type {} from '@codesmith/message-feedback/remote'
export type {} from '@codesmith/permission-presets/remote'
export type {} from '@codesmith/command-feedback/remote'
export type {} from '@codesmith/client-file-upload/remote'
export type {} from '@codesmith/session-reference/remote'
export type {} from '@codesmith/subagent/remote'
export type * from '@codesmith/subagent/client'
export type {} from '@codesmith/api-session-controller/remote'
export type * from '@codesmith/api-session-controller/types'
export type {} from '@codesmith/api-job-controller/remote'
export type * from '@codesmith/api-job-controller/types'
export type {} from '@codesmith/api-workspace-controller/remote'
export type * from '@codesmith/api-workspace-controller/types'
export type {} from '@codesmith/api-workspace-files/remote'
export type * from '@codesmith/api-workspace-files/types'
export type {} from '@codesmith/api-terminal-controller/remote'
export type * from '@codesmith/api-terminal-controller/types'
// The forwarded-event allowlist's selection seat: without it in the consumer's
// compilation face `TypertRemoteEvent` is `never` and every `$on` call fails.
export type { ApiRemoteForwardedEvent } from '../types.ts'
// The owner packages' client-safe `./types` exports supply the `Events`
// signatures `$on` hands to a listener, so a consumer reads the very
// declaration the Host emits rather than a flattened restatement of it.
export type {} from '@codesmith/commands/types'
export type {} from '@codesmith/web/types'
export type {} from '@codesmith/cordis-host-runner/types'
export type {} from '@codesmith/credentials/types'
export type {} from '@codesmith/llm/types'
export type {} from '@codesmith/agent-preset-registry/types'
export type {} from '@codesmith/permission-presets/types'
export type {} from '@codesmith/settings/types'
export type {} from '@codesmith/user-approval/types'
export type {} from '@codesmith/user-questions/types'
export type {} from '@codesmith/api-session-controller/types'

/**
 * The carrier's Client-facing types, re-exported so a business package names one
 * assembly package instead of both this facade and the Connection plugin. Type-only:
 * the carrier's runtime values stay behind their own module edge.
 */
export type {
  ConnectionHandle, ConnectionSinks, ContentBlock,
  MessageId,
  RpcId, RpcRequest, RpcResponse, RpcResult, SessionId,
  StreamChunk,
} from '@codesmith/client-connection/client'
export type {} from '@codesmith/api-gateway/client'
export type {} from '@codesmith/cordis-host-runner/remote'

// The payload vocabulary of the selected namespaces, re-exported so a Client
// contribution can name what it sends and receives without importing a Host
// package: this assembly is the one place both planes legitimately meet.
export type {
  ApprovalRequestId,
  CordisHalfState,
  CordisDynamicPackageId,
  CordisDynamicPluginId,
  CordisDynamicPluginRunId,
  CordisDynamicRunMode,
  CordisInspectMethodManifest,
  CordisInspectPlatform,
  CordisInspectProviderManifest,
  CordisInspectProviderView,
  CordisInspectQueryRequest,
  CordisInspectQueryResolution,
  CordisInspectQueryResolved,
  CordisInspectRequestId,
  CordisInspectResolveAck,
  CordisRunDiagnostic,
  CordisRunStatus,
  DynamicCordisClientSource,
  DynamicCordisHostHalfResult,
  DynamicCordisInventoryRow,
  DynamicCordisInvokeResult,
  DynamicCordisPackage,
  DynamicCordisRequestResolved,
  DynamicCordisResolveAck,
  DynamicCordisRetracted,
  DynamicCordisRunRequest,
  DynamicCordisRunResolution,
  DynamicCordisRunAttempt,
  DynamicCordisRunResponse,
  DynamicCordisStopResponse,
  DynamicCordisUndefineReceipt,
  RequestRunOutcome,
} from '@codesmith/cordis-host-runner/types'
// Credential state vocabulary for the credentials namespace (values never ride it).
export type { CredentialInfo } from '@codesmith/credentials/types'
// Redacted namespace vocabulary for the settings namespace (secrets never ride
// it). It travels with its seam, whose `./types` the Client face already reads.
export type {
  SettingsDescribeValue, SettingsNamespaceView, SettingsPathOpView, SettingsSecretView,
} from '@codesmith/settings/types'
// Provider registry and discovery vocabulary for the llm namespace.
export type {
  LlmConfigurableProvider, LlmDiscoveredModel,
  LlmModelDiscoveryRequest, LlmNativeAuth, LlmProviderInfo,
} from '@codesmith/llm/types'
// Reference-discovery result vocabulary for the fileReferences and
// sessionReferenceResolver namespaces.
export type { FileReferenceCandidate } from '@codesmith/file-reference/types'
export type { SessionReferenceMentionCandidate } from '@codesmith/session-reference/types'

// The Remote failure vocabulary, re-exported so business packages keep naming
// this assembly alone. Types only: a value export would make spec imports load
// this module's owner /remote artifacts; specs take RemoteError from
// cds-client-test-runtime instead.
export type {
  RemoteErrorCode, RemoteErrorDetailsMap, RemoteFailure, RemoteResult,
} from '@codesmith/typert-protocol'
export type { RemoteHostFacts } from '@codesmith/api-gateway/client'

declare module '@codesmith/cordis' {
  interface Context {
    /** Generated Remote namespaces selected by this Client assembly. */
    remote: ClientRemote
  }
}

/** Required service: the typed Client Remote contribution mount. */
export const inject = ['remote']

/**
 * Mount the Host capabilities explicitly selected for this Client assembly.
 * @param ctx - Client Cordis root carrying the typed API service.
 * @returns disposer after every selected Remote namespace is ready.
 */
export async function apply(ctx: Context): Promise<() => Promise<void>> {
  const disposers: Array<() => Promise<void>> = []
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
      disposers.push(await ctx.remote.$mount(contribution))
    }
  } catch (error) {
    for (const dispose of disposers.reverse()) await dispose()
    throw error
  }
  // Unwound in reverse mount order, so a namespace never outlives one mounted
  // after it.
  return async () => {
    for (const dispose of disposers.reverse()) await dispose()
  }
}
