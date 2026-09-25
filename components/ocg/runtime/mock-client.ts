import type {
  ChatMessage,
  ChatSession,
  Mission,
  OcgRuntimeEvent,
  RuntimeStatus,
  SendMessageInput,
  WorkType,
} from "../types";
import { createScenarioFixture } from "./scenarios";
import { boundTimeline } from "./observability";
import type {
  CreateSessionInput,
  OcgRuntimeClient,
  RuntimeSnapshot,
  ScenarioId,
} from "./runtime-types";

type Timer = ReturnType<typeof setTimeout>;

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function clockLabel(): string {
  const now = new Date();
  return `${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}`;
}

export class MockOcgRuntimeClient implements OcgRuntimeClient {
  private readonly scenario: ScenarioId;
  private readonly listeners = new Set<(event: OcgRuntimeEvent) => void>();
  private readonly timers = new Map<string, Timer[]>();
  private nextId = 0;
  private snapshot: RuntimeSnapshot;
  private liveScenarioStarted = false;

  constructor(scenario: ScenarioId) {
    const fixture = createScenarioFixture(scenario);
    this.scenario = scenario;
    this.snapshot = {
      scenario,
      status: clone(fixture.runtimeStatus),
      sessions: clone(fixture.sessions),
      messagesBySession: clone(fixture.messagesBySession),
      missionsBySession: clone(fixture.missionsBySession),
      observabilityBySession: clone(fixture.observabilityBySession),
    };
  }

  getSnapshot(): RuntimeSnapshot {
    return this.snapshot;
  }

  async getRuntimeStatus(): Promise<RuntimeStatus> {
    return clone(this.snapshot.status);
  }

  async listSessions(): Promise<ChatSession[]> {
    return clone(this.snapshot.sessions);
  }

  async getSession(id: string): Promise<ChatSession | null> {
    const session = this.snapshot.sessions.find((item) => item.id === id);
    return session ? clone(session) : null;
  }

  async getMessages(sessionId: string): Promise<ChatMessage[]> {
    return clone(this.snapshot.messagesBySession[sessionId] ?? []);
  }

  async getMission(sessionId: string): Promise<Mission | null> {
    return clone(this.snapshot.missionsBySession[sessionId] ?? null);
  }

  async getObservability(sessionId: string) {
    const observability = this.snapshot.observabilityBySession[sessionId];
    return observability ? clone(observability) : null;
  }

  async createSession(input: CreateSessionInput): Promise<ChatSession> {
    const id = `mock-session-${Date.now()}-${this.nextId++}`;
    const session: ChatSession = {
      id,
      title: input.title?.trim() || "Untitled thread",
      workType: input.workType,
      updatedAt: "now",
    };
    this.snapshot = {
      ...this.snapshot,
      sessions: [session, ...this.snapshot.sessions],
      messagesBySession: { ...this.snapshot.messagesBySession, [id]: [] },
      missionsBySession: { ...this.snapshot.missionsBySession, [id]: null },
      observabilityBySession: { ...this.snapshot.observabilityBySession, [id]: null },
    };
    this.emit({ type: "conversation.session-created", session: clone(session) });
    return clone(session);
  }

  async sendMessage(sessionId: string, input: SendMessageInput): Promise<void> {
    const content = input.content.trim();
    if (!content) return;

    if (this.snapshot.status.state !== "connected") {
      this.emit({
        type: "warning",
        message: this.snapshot.status.detail ?? "The local runtime is not connected.",
      });
      return;
    }

    const session = this.snapshot.sessions.find((item) => item.id === sessionId);
    if (!session) {
      this.emit({ type: "error", message: `Unknown mock session: ${sessionId}` });
      return;
    }

    const userMessage: ChatMessage = {
      id: `mock-user-${Date.now()}-${this.nextId++}`,
      role: "user",
      content,
      createdAt: clockLabel(),
      status: "completed",
    };
    this.updateMessages(sessionId, [...(this.snapshot.messagesBySession[sessionId] ?? []), userMessage]);

    const updatedSession = { ...session, updatedAt: "now" };
    this.snapshot = {
      ...this.snapshot,
      sessions: this.snapshot.sessions.map((item) => item.id === sessionId ? updatedSession : item),
    };
    this.emit({ type: "conversation.session-updated", session: clone(updatedSession) });

    const currentMission = this.snapshot.missionsBySession[sessionId];
    if (currentMission?.status === "running") {
      const updatedMission = { ...currentMission, current: "Responding to operator" };
      this.snapshot = {
        ...this.snapshot,
        missionsBySession: { ...this.snapshot.missionsBySession, [sessionId]: updatedMission },
      };
      this.emit({ type: "mission.updated", sessionId, mission: clone(updatedMission) });
    }

    const assistantId = `mock-assistant-${Date.now()}-${this.nextId++}`;
    const assistant: ChatMessage = {
      id: assistantId,
      role: "assistant",
      content: "",
      createdAt: clockLabel(),
      status: "streaming",
    };
    this.updateMessages(sessionId, [...(this.snapshot.messagesBySession[sessionId] ?? []), assistant]);
    this.emit({ type: "conversation.message-started", sessionId, message: clone(assistant) });

    const fixture = createScenarioFixture(this.scenario);
    const chunks = fixture.streamChunks ?? ["Mock runtime response."];
    const delay = fixture.streamDelayMs ?? 90;
    const timers: Timer[] = [];

    chunks.forEach((chunk, index) => {
      timers.push(setTimeout(() => {
        const current = this.snapshot.messagesBySession[sessionId]?.find((item) => item.id === assistantId);
        if (!current || current.status === "cancelled") return;
        const nextMessage = { ...current, content: current.content + chunk, status: "streaming" as const };
        this.replaceMessage(sessionId, nextMessage);
        this.emit({ type: "conversation.message-delta", sessionId, messageId: assistantId, delta: chunk });

        if (index === chunks.length - 1) {
          const completed = { ...nextMessage, status: "completed" as const };
          this.replaceMessage(sessionId, completed);
          this.emit({ type: "conversation.message-completed", sessionId, message: clone(completed) });
          this.timers.delete(sessionId);
        }
      }, delay * (index + 1)));
    });
    this.timers.set(sessionId, timers);
  }

  subscribe(listener: (event: OcgRuntimeEvent) => void): () => void {
    this.listeners.add(listener);
    this.startLiveScenario();
    return () => this.listeners.delete(listener);
  }

  async cancel(sessionId: string): Promise<void> {
    const timers = this.timers.get(sessionId) ?? [];
    timers.forEach((timer) => clearTimeout(timer));
    this.timers.delete(sessionId);
    const messages = this.snapshot.messagesBySession[sessionId] ?? [];
    const active = [...messages].reverse().find((message) => message.status === "streaming");
    if (active) {
      const cancelled = { ...active, status: "cancelled" as const };
      this.replaceMessage(sessionId, cancelled);
      this.emit({ type: "cancelled", sessionId, messageId: active.id });
    } else {
      this.emit({ type: "cancelled", sessionId });
    }
  }

  private updateMessages(sessionId: string, messages: ChatMessage[]): void {
    this.snapshot = {
      ...this.snapshot,
      messagesBySession: { ...this.snapshot.messagesBySession, [sessionId]: messages },
    };
  }

  private replaceMessage(sessionId: string, message: ChatMessage): void {
    const messages = this.snapshot.messagesBySession[sessionId] ?? [];
    this.updateMessages(sessionId, messages.map((item) => item.id === message.id ? message : item));
  }

  private emit(event: OcgRuntimeEvent): void {
    if (event.type === "runtime.status-changed") {
      this.snapshot = { ...this.snapshot, status: event.status };
    }
    if (event.type === "observability.updated") {
      this.snapshot = {
        ...this.snapshot,
        observabilityBySession: {
          ...this.snapshot.observabilityBySession,
          [event.sessionId]: { ...event.observability, timeline: boundTimeline(event.observability.timeline) },
        },
      };
    }
    for (const listener of this.listeners) listener(event);
  }

  private startLiveScenario(): void {
    if (this.scenario !== "observability-live" || this.liveScenarioStarted) return;
    this.liveScenarioStarted = true;
    const fixture = createScenarioFixture(this.scenario);
    fixture.observabilityUpdates?.forEach((update) => {
      const timer = setTimeout(() => {
        this.snapshot = {
          ...this.snapshot,
          missionsBySession: update.mission
            ? { ...this.snapshot.missionsBySession, [update.sessionId]: clone(update.mission) }
            : this.snapshot.missionsBySession,
        };
        this.emit({
          type: "observability.updated",
          sessionId: update.sessionId,
          observability: clone({ ...update.observability, timeline: boundTimeline(update.observability.timeline) }),
        });
        if (update.mission) this.emit({ type: "mission.updated", sessionId: update.sessionId, mission: clone(update.mission) });
      }, update.afterMs);
      const timers = this.timers.get("__observability__") ?? [];
      this.timers.set("__observability__", [...timers, timer]);
    });
  }
}

export function createMockOcgRuntimeClient(scenario: ScenarioId): OcgRuntimeClient {
  return new MockOcgRuntimeClient(scenario);
}

export type MockWorkType = WorkType;
