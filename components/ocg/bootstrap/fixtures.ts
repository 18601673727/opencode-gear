import type { ScenarioId } from "../runtime/runtime-types";
import type {
  BootstrapAccess,
  BootstrapAccessState,
  BootstrapCapability,
  BootstrapConnection,
  BootstrapFailure,
  BootstrapHandoffState,
  BootstrapMode,
  BootstrapModel,
  BootstrapOnboarding,
  BootstrapProfile,
  BootstrapResource,
  BootstrapState,
  OnboardingStageId,
} from "./types";

/**
 * Deterministic bootstrap fixtures. Every builder returns fresh objects so the
 * mock client can clone and mutate them without sharing state across scenarios.
 *
 * Only reachability and display metadata appear here. No credential material is
 * modeled, stored, or implied.
 */

function localAccess(): BootstrapAccess {
  return {
    state: "local",
    remote: false,
    handoffState: "unavailable",
    detail: "Local workspace.",
  };
}

function remoteAccess(
  state: BootstrapAccessState,
  handoffState: BootstrapHandoffState,
  detail: string,
): BootstrapAccess {
  return { state, remote: true, handoffState, detail };
}

function baseResources(): BootstrapResource[] {
  return [
    {
      id: "workspace-primary",
      label: "OCG workspace",
      kind: "workspace",
      source: "local",
      status: "available",
      detail: "Primary workspace detected.",
      economics: { kind: "unknown", detail: "Workspace accounting is not applicable." },
      capacity: { status: "unknown", detail: "Not reported by the local runtime." },
      required: true,
    },
    {
      id: "repo-runtime",
      label: "Durable mission runtime",
      kind: "repository",
      source: "local",
      status: "available",
      detail: "Repository detected in the workspace.",
      economics: { kind: "unknown", detail: "No provider billing data." },
      capacity: { status: "unknown", detail: "Not reported by the local runtime." },
      required: false,
    },
    {
      id: "endpoint-broker",
      label: "Resource broker endpoint",
      kind: "endpoint",
      source: "remote",
      status: "unknown",
      detail: "Not exposed by the current runtime.",
      provider: "Future provider",
      economics: { kind: "unknown", detail: "Pricing not reported." },
      capacity: { status: "unknown", detail: "Capacity not reported." },
      required: false,
    },
    {
      id: "openai-subscription",
      label: "OpenAI subscription",
      kind: "endpoint",
      source: "remote",
      status: "available",
      provider: "OpenAI",
      plan: "Subscription resource",
      detail: "Detected from the connected runtime.",
      economics: { kind: "subscription", detail: "Subscription; per-request price is unknown." },
      capacity: { status: "unknown", detail: "Quota was not reported." },
      required: false,
    },
    {
      id: "goat-command-code",
      label: "GOAT / Command Code",
      kind: "endpoint",
      source: "remote",
      status: "available",
      provider: "Command Code",
      plan: "Connected account",
      detail: "Available through the runtime auth mechanism.",
      economics: { kind: "subscription", detail: "Plan economics are provider-owned." },
      capacity: { status: "known", detail: "Shared capacity; exact quota is not shown." },
      required: false,
    },
    {
      id: "opencode-go-free",
      label: "OpenCode Go",
      kind: "endpoint",
      source: "remote",
      status: "available",
      provider: "OpenCode Go",
      plan: "Free / low-cost resource",
      detail: "A lower-cost fallback is available.",
      economics: { kind: "free", detail: "Free or included usage; provider limits apply." },
      capacity: { status: "known", detail: "Best-effort capacity." },
      required: false,
    },
    {
      id: "openai-compatible-endpoint",
      label: "OpenAI-compatible endpoint",
      kind: "endpoint",
      source: "imported",
      status: "available",
      provider: "OpenAI-compatible",
      plan: "Imported endpoint",
      detail: "Endpoint shape detected; provider metadata is limited.",
      economics: { kind: "unknown", detail: "Pricing not reported." },
      capacity: { status: "unknown", detail: "Capacity not reported." },
      required: false,
    },
    {
      id: "temporarily-unavailable-provider",
      label: "Additional provider resource",
      kind: "endpoint",
      source: "remote",
      status: "unavailable",
      provider: "Unreported provider",
      plan: "Unknown plan",
      detail: "Temporarily unavailable; it will not be used for the recommendation.",
      economics: { kind: "unknown", detail: "Pricing not reported." },
      capacity: { status: "unknown", detail: "Capacity not reported." },
      required: false,
    },
  ];
}

function baseConnections(): BootstrapConnection[] {
  return [
    {
      id: "runtime-local",
      label: "Local runtime",
      kind: "runtime",
      state: "connected",
      detail: "Local mock runtime.",
      requiresHandoff: false,
      required: true,
    },
    {
      id: "provider-local",
      label: "Model provider",
      kind: "model-provider",
      state: "connected",
      detail: "Connected for this workspace.",
      requiresHandoff: false,
      required: true,
    },
    {
      id: "source-control",
      label: "Source control",
      kind: "source-control",
      state: "unconfigured",
      detail: "Optional in this phase.",
      requiresHandoff: false,
      required: false,
      advanced: true,
    },
    {
      id: "provider-secondary",
      label: "Additional provider account",
      kind: "model-provider",
      state: "auth-required",
      detail: "Authentication is delegated to the provider runtime.",
      requiresHandoff: false,
      required: false,
    },
  ];
}

function baseModels(): BootstrapModel[] {
  return [
    {
      id: "zen-muse",
      provider: "OpenCode Zen",
      model: "Muse Spark 1.3 Contributor Free",
      status: "available",
      capabilities: ["streaming", "tools"],
    },
    {
      id: "command-muse",
      provider: "Command Code",
      model: "Muse Spark 1.3 Contributor",
      status: "available",
      capabilities: ["streaming", "tools", "reasoning"],
    },
  ];
}

function baseCapabilities(): BootstrapCapability[] {
  return [
    { id: "streaming", label: "Streaming responses", status: "available" },
    { id: "tools", label: "Tool activity", status: "available" },
    { id: "workers", label: "Parallel workers", status: "available" },
    { id: "vision", label: "Image input", status: "unknown" },
  ];
}

function baseProfiles(): BootstrapProfile[] {
  return [
    {
      id: "economy",
      label: "Lean",
      tier: "economy",
      rationale: "Lowest expected spend for routine threads.",
      modelIds: ["zen-muse"],
      recommended: false,
    },
    {
      id: "balanced",
      label: "Balanced",
      tier: "balanced",
      rationale: "Balances capability and spend for most work.",
      modelIds: ["command-muse", "zen-muse"],
      recommended: true,
    },
    {
      id: "quality",
      label: "Quality-first",
      tier: "quality",
      rationale: "Prefers the stronger available model per task.",
      modelIds: ["command-muse"],
      recommended: false,
    },
  ];
}

function localReady(): BootstrapState {
  return {
    access: localAccess(),
    onboarding: null,
    resources: baseResources(),
    connections: baseConnections(),
    models: baseModels(),
    capabilities: baseCapabilities(),
    profiles: baseProfiles(),
    ready: true,
  };
}

function onboardingState(
  mode: BootstrapMode,
  stage: OnboardingStageId,
  completedStages: OnboardingStageId[],
  options: { canResume?: boolean; failure?: BootstrapFailure } = {},
): BootstrapOnboarding {
  return {
    mode,
    stage,
    completedStages,
    canResume: options.canResume ?? false,
    ...(options.failure ? { failure: options.failure } : {}),
  };
}

function configured(access: BootstrapAccess, onboarding: BootstrapOnboarding | null): BootstrapState {
  return {
    access,
    onboarding,
    resources: baseResources(),
    connections: baseConnections(),
    models: baseModels(),
    capabilities: baseCapabilities(),
    profiles: baseProfiles(),
    ready: false,
  };
}

function remoteReady(access: BootstrapAccess): BootstrapState {
  const state = localReady();
  return { ...state, access };
}

function connectionFailure(): BootstrapFailure {
  return {
    code: "connection-failed",
    summary: "The model provider connection could not be established.",
    detail: "The last connection attempt did not complete. No request left this browser.",
    action: "retry",
    actionLabel: "Retry connection",
    retryable: true,
  };
}

function recoveryFailure(): BootstrapFailure {
  return {
    code: "recovery-failed",
    summary: "The previous setup could not be restored.",
    detail: "A saved setup checkpoint is available for the mock restore action.",
    action: "restore",
    actionLabel: "Restore saved setup",
    retryable: true,
  };
}

function invalidConfigurationFailure(): BootstrapFailure {
  return {
    code: "invalid-configuration",
    summary: "The saved setup is not valid for this runtime.",
    detail: "Review the detected resources and choose a new recommendation before continuing.",
    action: "reconfigure",
    actionLabel: "Review setup",
    retryable: false,
  };
}

function onboardingResume(): BootstrapState {
  const connections = baseConnections().map((connection) =>
    connection.id === "source-control" ? { ...connection, state: "pending" as const } : connection,
  );
  return {
    ...configured(
      localAccess(),
      onboardingState("resume", "connections", ["welcome", "resources"], { canResume: true }),
    ),
    connections,
  };
}

function onboardingMigration(): BootstrapState {
  const resources = baseResources().map((resource) =>
    resource.id === "repo-runtime"
      ? { ...resource, source: "imported" as const, status: "pending" as const, detail: "Imported from the previous setup." }
      : resource,
  );
  return {
    ...configured(localAccess(), onboardingState("migrate", "resources", ["welcome"], { canResume: true })),
    resources,
  };
}

function onboardingRecovery(): BootstrapState {
  return configured(localAccess(), onboardingState("recover", "welcome", [], { failure: recoveryFailure() }));
}

function onboardingInvalidConfiguration(): BootstrapState {
  return configured(
    localAccess(),
    onboardingState("reconfigure", "welcome", [], { failure: invalidConfigurationFailure() }),
  );
}

function onboardingAuthRequired(): BootstrapState {
  return configured(
    remoteAccess("auth-required", "idle", "Access is required before setup can continue."),
    onboardingState("firstRun", "welcome", []),
  );
}

function onboardingConnectionFailure(): BootstrapState {
  const connections = baseConnections().map((connection) =>
    connection.id === "provider-local"
      ? { ...connection, state: "failed" as const, detail: "The last attempt did not complete." }
      : connection,
  );
  return {
    ...configured(
      localAccess(),
      onboardingState("reconfigure", "connections", ["welcome", "resources"], { failure: connectionFailure() }),
    ),
    connections,
  };
}

function onboardingDiscovery(): BootstrapState {
  const models: BootstrapModel[] = [
    ...baseModels().slice(0, 1),
    {
      id: "unknown-provider-model",
      provider: "Unconfigured provider",
      model: "Not reported",
      status: "unknown",
      capabilities: [],
    },
  ];
  const capabilities: BootstrapCapability[] = baseCapabilities().map((capability) => ({
    ...capability,
    status: "unknown" as const,
  }));
  return {
    ...configured(localAccess(), onboardingState("firstRun", "discovery", ["welcome", "resources", "connections"])),
    models,
    capabilities,
  };
}

function onboardingReady(): BootstrapState {
  return configured(
    localAccess(),
    onboardingState(
      "firstRun",
      "ready",
      ["welcome", "resources", "connections", "discovery", "overview", "profile"],
      { canResume: true },
    ),
  );
}

export function createBootstrapFixture(scenario: ScenarioId): BootstrapState {
  switch (scenario) {
    case "local-ready":
      return localReady();
    case "local-first-run":
      return configured(localAccess(), onboardingState("firstRun", "welcome", []));
    case "remote-unauthenticated":
      return remoteReady(remoteAccess("unauthenticated", "idle", "Access handoff is required for this remote workspace."));
    case "remote-session-expired":
      return remoteReady(remoteAccess("session-expired", "idle", "The access session expired. Refresh the handoff to continue."));
    case "remote-denied":
      return remoteReady(remoteAccess("denied", "failed", "Access to this workspace was denied."));
    case "remote-authenticated-ready":
      return remoteReady(remoteAccess("authenticated", "complete", "Access handoff granted (mock)."));
    case "remote-authenticated-first-run":
      return configured(remoteAccess("authenticated", "complete", "Access handoff granted (mock)."), onboardingState("firstRun", "welcome", []));
    case "onboarding-resume":
      return onboardingResume();
    case "onboarding-migration":
      return onboardingMigration();
    case "onboarding-recovery":
      return onboardingRecovery();
    case "onboarding-invalid-configuration":
      return onboardingInvalidConfiguration();
    case "onboarding-auth-required":
      return onboardingAuthRequired();
    case "onboarding-connection-failure":
      return onboardingConnectionFailure();
    case "onboarding-discovery":
      return onboardingDiscovery();
    case "onboarding-ready":
      return onboardingReady();
    default:
      // Every pre-existing scenario predates remote access and stays local-ready.
      return localReady();
  }
}
