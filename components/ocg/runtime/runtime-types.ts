import type {
  ChatMessage,
  ChatSession,
  Mission,
  OcgRuntimeEvent,
  RuntimeStatus,
  SendMessageInput,
} from "../types";
import type { RuntimeObservability } from "./observability";
import type { ResourceLedger } from "../resource-ledger/types";
import type { BootstrapState, OnboardingStageId } from "../bootstrap/types";

export type ScenarioId =
  | "normal-chat"
  | "long-stream"
  | "tool-heavy"
  | "worker-parallel"
  | "build-failed"
  | "retry-success"
  | "mission-complete"
  | "budget-exhausted"
  | "runtime-disconnected"
  | "runtime-connecting"
  | "runtime-failed"
  | "permission-required"
  | "observability-live"
  | "resource-ledger"
  | "local-ready"
  | "local-first-run"
  | "remote-unauthenticated"
  | "remote-session-expired"
  | "remote-denied"
  | "remote-authenticated-ready"
  | "remote-authenticated-first-run"
  | "onboarding-resume"
  | "onboarding-migration"
  | "onboarding-recovery"
  | "onboarding-invalid-configuration"
  | "onboarding-auth-required"
  | "onboarding-connection-failure"
  | "onboarding-discovery"
  | "onboarding-ready";

export type RuntimeSnapshot = {
  scenario: ScenarioId;
  status: RuntimeStatus;
  sessions: ChatSession[];
  messagesBySession: Record<string, ChatMessage[]>;
  missionsBySession: Record<string, Mission | null>;
  observabilityBySession: Record<string, RuntimeObservability | null>;
  resourceLedger: ResourceLedger | null;
  bootstrap: BootstrapState;
};

export type CreateSessionInput = {
  title?: string;
  workType: ChatSession["workType"];
};

export interface OcgRuntimeClient {
  getRuntimeStatus(): Promise<RuntimeStatus>;
  listSessions(): Promise<ChatSession[]>;
  getSession(id: string): Promise<ChatSession | null>;
  getMessages(sessionId: string): Promise<ChatMessage[]>;
  getMission(sessionId: string): Promise<Mission | null>;
  getObservability(sessionId: string): Promise<RuntimeObservability | null>;
  getBootstrap(): Promise<BootstrapState>;
  createSession(input: CreateSessionInput): Promise<ChatSession>;
  sendMessage(sessionId: string, input: SendMessageInput): Promise<void>;
  subscribe(listener: (event: OcgRuntimeEvent) => void): () => void;
  getSnapshot(): RuntimeSnapshot;
  cancel?(sessionId: string): Promise<void>;
  /** Mock Cloudflare Access handoff. Never a real redirect or credential exchange. */
  requestAccessHandoff?(): Promise<void>;
  setOnboardingStage?(stage: OnboardingStageId): Promise<void>;
  completeOnboarding?(): Promise<void>;
  /** Mock recovery for an actionable bootstrap failure. */
  retryBootstrap?(): Promise<void>;
}
