"use client";

import { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import { MockOcgRuntimeClient } from "./mock-client";
import type { CreateSessionInput, OcgRuntimeClient, ScenarioId } from "./runtime-types";
import type { ChatSession, SendMessageInput } from "../types";
import type { RuntimeSnapshot } from "./runtime-types";

type RuntimeContextValue = {
  client: OcgRuntimeClient;
  snapshot: RuntimeSnapshot;
  createSession: (input: CreateSessionInput) => Promise<ChatSession>;
  sendMessage: (sessionId: string, input: SendMessageInput) => Promise<void>;
  cancel: (sessionId: string) => Promise<void>;
};

const RuntimeContext = createContext<RuntimeContextValue | null>(null);

export function OcgRuntimeProvider({ scenario, children }: { scenario: ScenarioId; children: ReactNode }) {
  const client = useMemo(() => new MockOcgRuntimeClient(scenario), [scenario]);
  const subscribe = useCallback(
    (onStoreChange: () => void) => client.subscribe(() => onStoreChange()),
    [client],
  );
  const getSnapshot = useCallback(() => client.getSnapshot(), [client]);
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);

  const createSession = useCallback(async (input: CreateSessionInput) => {
    return client.createSession(input);
  }, [client]);
  const sendMessage = useCallback(async (sessionId: string, input: SendMessageInput) => {
    await client.sendMessage(sessionId, input);
  }, [client]);
  const cancel = useCallback(async (sessionId: string) => {
    await client.cancel?.(sessionId);
  }, [client]);

  const value = useMemo(
    () => ({ client, snapshot, createSession, sendMessage, cancel }),
    [cancel, client, createSession, sendMessage, snapshot],
  );
  return <RuntimeContext.Provider value={value}>{children}</RuntimeContext.Provider>;
}

export function useOcgRuntime(): RuntimeContextValue {
  const context = useContext(RuntimeContext);
  if (!context) throw new Error("useOcgRuntime must be used inside OcgRuntimeProvider");
  return context;
}
