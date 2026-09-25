"use client";

import { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from "react";
import type { ReactNode } from "react";
import { MockOcgRuntimeClient } from "./mock-client";
import type { CreateSessionInput, OcgRuntimeClient, ScenarioId } from "./runtime-types";
import type { ChatSession, SendMessageInput } from "../types";
import type { RuntimeSnapshot } from "./runtime-types";
import type { OnboardingStageId } from "../bootstrap/types";

type RuntimeContextValue = {
  client: OcgRuntimeClient;
  snapshot: RuntimeSnapshot;
  createSession: (input: CreateSessionInput) => Promise<ChatSession>;
  sendMessage: (sessionId: string, input: SendMessageInput) => Promise<void>;
  cancel: (sessionId: string) => Promise<void>;
  requestAccessHandoff: () => Promise<void>;
  setOnboardingStage: (stage: OnboardingStageId) => Promise<void>;
  completeOnboarding: () => Promise<void>;
  retryBootstrap: () => Promise<void>;
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
  const requestAccessHandoff = useCallback(async () => {
    await client.requestAccessHandoff?.();
  }, [client]);
  const setOnboardingStage = useCallback(async (stage: OnboardingStageId) => {
    await client.setOnboardingStage?.(stage);
  }, [client]);
  const completeOnboarding = useCallback(async () => {
    await client.completeOnboarding?.();
  }, [client]);
  const retryBootstrap = useCallback(async () => {
    await client.retryBootstrap?.();
  }, [client]);

  const value = useMemo(
    () => ({
      client,
      snapshot,
      createSession,
      sendMessage,
      cancel,
      requestAccessHandoff,
      setOnboardingStage,
      completeOnboarding,
      retryBootstrap,
    }),
    [cancel, client, completeOnboarding, createSession, requestAccessHandoff, retryBootstrap, sendMessage, setOnboardingStage, snapshot],
  );
  return <RuntimeContext.Provider value={value}>{children}</RuntimeContext.Provider>;
}

export function useOcgRuntime(): RuntimeContextValue {
  const context = useContext(RuntimeContext);
  if (!context) throw new Error("useOcgRuntime must be used inside OcgRuntimeProvider");
  return context;
}
