import type {
  ChatMessage,
  ChatSession,
  Mission,
  RuntimeStatus,
  ToolActivity,
} from "../types";
import type { ScenarioId } from "./runtime-types";

export const DEFAULT_SCENARIO: ScenarioId = "normal-chat";
export const SCENARIO_IDS: readonly ScenarioId[] = [
  "normal-chat",
  "long-stream",
  "tool-heavy",
  "worker-parallel",
  "build-failed",
  "retry-success",
  "mission-complete",
  "budget-exhausted",
  "runtime-disconnected",
  "runtime-connecting",
  "runtime-failed",
  "permission-required",
];

export function resolveScenario(value: string | undefined | null): ScenarioId {
  return SCENARIO_IDS.includes(value as ScenarioId) ? (value as ScenarioId) : DEFAULT_SCENARIO;
}

const baseSession: ChatSession = {
  id: "design-pwa-shell",
  title: "OCG PWA shell",
  workType: "design",
  updatedAt: "now",
};

const sessions: ChatSession[] = [
  baseSession,
  { id: "research-space-bunny", title: "Space Bunny architecture study", workType: "research", updatedAt: "2h ago" },
  { id: "research-rust-graph", title: "Rust graph storage options", workType: "research", updatedAt: "1d ago" },
  { id: "coding-mission-runtime", title: "OCG durable mission runtime", workType: "coding", updatedAt: "12m ago" },
  { id: "coding-tool-gateway", title: "Tool Call Gateway", workType: "coding", updatedAt: "3h ago" },
  { id: "design-resource-controls", title: "Mission resource controls", workType: "design", updatedAt: "2d ago" },
  { id: "devops-debian-runtime", title: "Debian runtime setup", workType: "devops", updatedAt: "5h ago" },
  { id: "devops-cloudflare-access", title: "Cloudflare remote access", workType: "devops", updatedAt: "4d ago" },
];

function message(
  id: string,
  role: ChatMessage["role"],
  content: string,
  status: ChatMessage["status"] = "completed",
  tool?: ToolActivity,
): ChatMessage {
  return { id, role, content, status, createdAt: "10:03", ...(tool ? { tool } : {}) };
}

function tool(id: string, status: ToolActivity["status"], name: string, summary: string): ToolActivity {
  return {
    id,
    name,
    status,
    durationMs: status === "running" ? 0 : 184,
    summary,
    detail: `scenario fixture: ${name}\nresult: ${status}`,
    ...(status === "retrying" ? { retryCount: 1 } : {}),
  };
}

function normalMessages(): ChatMessage[] {
  return [
    message("normal-u1", "user", "What should we preserve while the runtime boundary is still local?"),
    message(
      "normal-a1",
      "assistant",
      "Keep the shell, conversation state, and Mission state separate. The mock runtime owns the data today, so a future transport can replace it without changing the panels.",
    ),
  ];
}

function genericMessages(session: ChatSession): ChatMessage[] {
  return [
    message(
      `${session.id}-u1`,
      "user",
      `Continuing "${session.title}". What is the current open question for this ${session.workType} thread?`,
    ),
    message(
      `${session.id}-a1`,
      "assistant",
      `Open question for **${session.title}**:\n\n- Scope the smallest deliverable first.\n- Keep UI state local until the runtime contract is frozen.\n- Record decisions in the thread for the later durable Mission.\n\nThis is mock content — no backend is connected in this phase.`,
    ),
  ];
}

function mission(status: Mission["status"], overrides: Partial<Mission> = {}): Mission {
  return {
    title: "Build OCG PWA shell",
    goal: "Exercise the frontend runtime boundary with a realistic Mission surface.",
    status,
    completed: 3,
    total: 7,
    current: "Implement runtime abstraction",
    tasks: [
      { id: "t1", title: "Establish project baseline", status: "completed" },
      { id: "t2", title: "Build application shell", status: "completed" },
      { id: "t3", title: "Define frontend domain", status: "completed" },
      { id: "t4", title: "Exercise mock runtime", status: "active" },
      { id: "t5", title: "Connect future transport", status: "pending" },
      { id: "t6", title: "Add durable persistence", status: "pending" },
      { id: "t7", title: "Ship runtime integration", status: "pending" },
    ],
    workers: [{ id: "ocg-local", name: "ocg-local", status: "idle", task: "Waiting" }],
    elapsed: "42m",
    commitment: { workers: 2, mode: "capped" },
    budget: { spent: 4.2, limit: 25, currency: "USD", status: "within-limit" },
    warnings: [],
    ...overrides,
  };
}

export type ScenarioFixture = {
  id: ScenarioId;
  runtimeStatus: RuntimeStatus;
  sessions: ChatSession[];
  messagesBySession: Record<string, ChatMessage[]>;
  missionsBySession: Record<string, Mission | null>;
  streamChunks?: string[];
  streamDelayMs?: number;
};

export function createScenarioFixture(id: ScenarioId): ScenarioFixture {
  const fixture: ScenarioFixture = {
    id,
    runtimeStatus: { state: "connected", detail: "local mock runtime" },
    sessions,
    messagesBySession: Object.fromEntries(
      sessions.map((session) => [session.id, session.id === baseSession.id ? normalMessages() : genericMessages(session)]),
    ),
    missionsBySession: Object.fromEntries(sessions.map((session) => [session.id, mission("running")])),
    streamChunks: [
      "Mock runtime received your message. ",
      "This response is streamed locally, ",
      "one deterministic chunk at a time.",
    ],
    streamDelayMs: 90,
  };

  switch (id) {
    case "long-stream":
      fixture.messagesBySession[baseSession.id] = [
        message("stream-u1", "user", "Show me the runtime state as it arrives."),
        message("stream-a1", "assistant", "The stream is still arriving…", "streaming"),
      ];
      fixture.streamChunks = [
        "The stream is still arriving… ",
        "Each delta updates the frontend-owned message, ",
        "without exposing transport event names to ChatView.",
      ];
      fixture.streamDelayMs = 180;
      break;
    case "tool-heavy":
      fixture.messagesBySession[baseSession.id] = [
        message("tool-u1", "user", "Inspect the workspace and validate the shell."),
        message("tool-t1", "tool", "", "completed", tool("tool-t1", "success", "workspace.read", "Read package and shell files")),
        message("tool-t2", "tool", "", "completed", tool("tool-t2", "running", "pnpm lint", "Checking frontend boundaries")),
        message("tool-t3", "tool", "", "completed", tool("tool-t3", "retrying", "build.verify", "Retrying after a transient failure")),
        message("tool-a1", "assistant", "The mock runtime can show several activities without coupling the UI to a backend protocol."),
      ];
      break;
    case "worker-parallel":
      fixture.missionsBySession[baseSession.id] = mission("running", {
        current: "Parallel workers are evaluating shell boundaries",
        workers: [
          { id: "worker-research", name: "researcher", status: "active", task: "Compare runtime contracts" },
          { id: "worker-build", name: "builder", status: "active", task: "Exercise fixture states" },
          { id: "worker-review", name: "reviewer", status: "completed", task: "Review component boundaries" },
        ],
        commitment: { workers: 3, mode: "capped" },
      });
      break;
    case "build-failed":
      fixture.runtimeStatus = { state: "failed", detail: "Build verification reported a fixture failure" };
      fixture.missionsBySession[baseSession.id] = mission("failed", {
        current: "Build verification failed",
        tasks: fixture.missionsBySession[baseSession.id]!.tasks.map((task) =>
          task.id === "t4" ? { ...task, status: "failed" } : task,
        ),
        workers: [{ id: "builder", name: "builder", status: "failed", task: "Build verification" }],
        warnings: ["The fixture represents a failed build; no command was executed."],
      });
      fixture.messagesBySession[baseSession.id] = [
        message("failed-u1", "user", "Run the build verification."),
        message("failed-t1", "tool", "", "failed", tool("failed-t1", "failure", "build.verify", "Build failed with fixture error")),
        message("failed-a1", "assistant", "The build step failed in this scenario. Retry is available from the future runtime.", "failed"),
      ];
      break;
    case "retry-success":
      fixture.missionsBySession[baseSession.id] = mission("running", {
        warnings: ["One activity succeeded after a retry."],
        current: "Continue after successful retry",
      });
      fixture.messagesBySession[baseSession.id] = [
        message("retry-u1", "user", "Retry the failed build."),
        message("retry-t1", "tool", "", "completed", tool("retry-t1", "success", "build.verify", "Succeeded on retry")),
        message("retry-a1", "assistant", "The retry succeeded. The Mission can continue without hiding the earlier failure."),
      ];
      break;
    case "mission-complete":
      fixture.missionsBySession[baseSession.id] = mission("completed", {
        completed: 7,
        current: "Mission complete",
        tasks: fixture.missionsBySession[baseSession.id]!.tasks.map((task) => ({ ...task, status: "completed" })),
        workers: [{ id: "ocg-local", name: "ocg-local", status: "completed", task: "All tasks complete" }],
      });
      break;
    case "budget-exhausted":
      fixture.missionsBySession[baseSession.id] = mission("budget-exhausted", {
        current: "Budget exhausted before the next task",
        budget: { spent: 25, limit: 25, currency: "USD", status: "exhausted" },
        warnings: ["Hard budget reached. No additional work will be started."],
      });
      break;
    case "runtime-disconnected":
      fixture.runtimeStatus = { state: "disconnected", detail: "No local runtime is connected" };
      fixture.missionsBySession[baseSession.id] = null;
      break;
    case "runtime-connecting":
      fixture.runtimeStatus = { state: "connecting", detail: "Connecting to the local mock runtime" };
      fixture.missionsBySession[baseSession.id] = null;
      break;
    case "runtime-failed":
      fixture.runtimeStatus = { state: "failed", detail: "The local mock runtime failed to start" };
      fixture.missionsBySession[baseSession.id] = null;
      break;
    case "permission-required":
      fixture.messagesBySession[baseSession.id] = [
        message("permission-u1", "user", "Apply the proposed workspace change."),
        message("permission-t1", "tool", "", "pending", tool("permission-t1", "waiting-approval", "workspace.write", "Waiting for operator approval")),
        message("permission-a1", "assistant", "The next activity is waiting for approval before it can continue."),
      ];
      fixture.missionsBySession[baseSession.id] = mission("paused", { current: "Waiting for operator approval" });
      break;
  }

  return fixture;
}
