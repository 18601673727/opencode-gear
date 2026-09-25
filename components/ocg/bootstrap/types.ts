/**
 * Normalized bootstrap domain for the mock runtime.
 *
 * This module is deliberately presentation-free and secret-free. It owns the
 * setup facts the shell needs before it can show the workspace: how access is
 * established, whether onboarding is pending, and the resource-first inventory
 * used by the wizard. Connections describe reachability only; no credential
 * material is ever represented here.
 */

export type BootstrapEntry = "app" | "login" | "onboarding";

export type BootstrapMode = "firstRun" | "resume" | "migrate" | "recover" | "reconfigure";

export const BOOTSTRAP_MODES: readonly BootstrapMode[] = [
  "firstRun",
  "resume",
  "migrate",
  "recover",
  "reconfigure",
];

export type BootstrapAccessState =
  | "local"
  | "authenticated"
  | "unauthenticated"
  | "session-expired"
  | "denied"
  | "auth-required";

export type BootstrapHandoffState = "unavailable" | "idle" | "pending" | "complete" | "failed";

export type BootstrapAccess = {
  state: BootstrapAccessState;
  remote: boolean;
  detail?: string;
  handoffState: BootstrapHandoffState;
};

export type OnboardingStageId =
  | "welcome"
  | "resources"
  | "connections"
  | "discovery"
  | "overview"
  | "profile"
  | "ready";

/** Canonical stage order for the seven-stage wizard. */
export const ONBOARDING_STAGES: readonly OnboardingStageId[] = [
  "welcome",
  "resources",
  "connections",
  "discovery",
  "overview",
  "profile",
  "ready",
];

export type BootstrapFailureCode =
  | "auth-required"
  | "connection-failed"
  | "discovery-failed"
  | "invalid-configuration"
  | "migration-failed"
  | "recovery-failed"
  | "unknown";

/** How the operator is expected to resolve a failure. */
export type BootstrapFailureAction =
  | "handoff"
  | "retry"
  | "resume"
  | "restore"
  | "reconfigure"
  | "continue";

export type BootstrapFailure = {
  code: BootstrapFailureCode;
  summary: string;
  /** Optional, display-safe technical context. Never secret material. */
  detail?: string;
  action: BootstrapFailureAction;
  actionLabel: string;
  retryable: boolean;
};

export type BootstrapResourceKind =
  | "workspace"
  | "project"
  | "repository"
  | "directory"
  | "endpoint"
  | "unknown";

export type BootstrapEconomicsKind = "free" | "subscription" | "metered" | "unknown";

export type BootstrapResourceEconomics = {
  kind: BootstrapEconomicsKind;
  detail?: string;
};

export type BootstrapResourceCapacity = {
  status: "known" | "unknown";
  detail?: string;
};

export type BootstrapResourceStatus = "available" | "pending" | "unavailable" | "denied" | "unknown";

export type BootstrapResource = {
  id: string;
  label: string;
  kind: BootstrapResourceKind;
  source: "local" | "remote" | "imported";
  status: BootstrapResourceStatus;
  detail?: string;
  provider?: string;
  plan?: string;
  economics: BootstrapResourceEconomics;
  capacity: BootstrapResourceCapacity;
  required: boolean;
};

export type BootstrapConnectionKind =
  | "runtime"
  | "model-provider"
  | "source-control"
  | "workspace"
  | "unknown";

export type BootstrapConnectionState =
  | "connected"
  | "pending"
  | "unconfigured"
  | "auth-required"
  | "failed"
  | "denied"
  | "unknown";

export type BootstrapConnection = {
  id: string;
  label: string;
  kind: BootstrapConnectionKind;
  state: BootstrapConnectionState;
  detail?: string;
  /** True when completing the connection requires a browser access handoff. */
  requiresHandoff: boolean;
  required: boolean;
  advanced?: boolean;
};

export type BootstrapModelStatus = "available" | "pending" | "unavailable" | "unknown";

export type BootstrapModel = {
  id: string;
  provider: string;
  model: string;
  status: BootstrapModelStatus;
  capabilities: string[];
};

export type BootstrapCapabilityStatus = "available" | "unknown";

export type BootstrapCapability = {
  id: string;
  label: string;
  status: BootstrapCapabilityStatus;
};

export type BootstrapProfileTier = "economy" | "balanced" | "quality";

export type BootstrapProfile = {
  id: string;
  label: string;
  tier: BootstrapProfileTier;
  rationale: string;
  modelIds: string[];
  recommended: boolean;
  advanced?: boolean;
};

export type BootstrapOnboarding = {
  mode: BootstrapMode;
  stage: OnboardingStageId;
  completedStages: OnboardingStageId[];
  canResume: boolean;
  failure?: BootstrapFailure;
};

export type BootstrapState = {
  access: BootstrapAccess;
  onboarding: BootstrapOnboarding | null;
  resources: BootstrapResource[];
  connections: BootstrapConnection[];
  models: BootstrapModel[];
  capabilities: BootstrapCapability[];
  profiles: BootstrapProfile[];
  ready: boolean;
};

export type StageGate = {
  stage: OnboardingStageId;
  canAdvance: boolean;
  /** Stable codes explaining why advancement is blocked. */
  blockers: string[];
};
