import type {
  ChatMessage,
  ChatSession,
  Mission,
  OcgRuntimeEvent,
  RuntimeStatus,
  SendMessageInput,
} from "../types";

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
  | "permission-required";

export type RuntimeSnapshot = {
  scenario: ScenarioId;
  status: RuntimeStatus;
  sessions: ChatSession[];
  messagesBySession: Record<string, ChatMessage[]>;
  missionsBySession: Record<string, Mission | null>;
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
  createSession(input: CreateSessionInput): Promise<ChatSession>;
  sendMessage(sessionId: string, input: SendMessageInput): Promise<void>;
  subscribe(listener: (event: OcgRuntimeEvent) => void): () => void;
  getSnapshot(): RuntimeSnapshot;
  cancel?(sessionId: string): Promise<void>;
}
