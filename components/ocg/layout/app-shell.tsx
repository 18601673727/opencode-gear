"use client";

import { useCallback, useEffect, useState } from "react";
import { cn } from "@/lib/utils";
import { ChatView } from "../chat/chat-view";
import { MissionView } from "../mission/mission-view";
import { OcgSidebar } from "../sidebar/ocg-sidebar";
import { OcgTopbar } from "../topbar/ocg-topbar";
import { OcgRuntimeProvider, useOcgRuntime } from "../runtime/runtime-context";
import type { ScenarioId } from "../runtime/runtime-types";
import type { InspectorMode } from "../observability/inspector-state";

export function AppShell({ scenario }: { scenario: ScenarioId }) {
  return (
    <OcgRuntimeProvider scenario={scenario}>
      <RuntimeWorkspace />
    </OcgRuntimeProvider>
  );
}

export function RuntimeWorkspace() {
  const { snapshot, createSession, sendMessage } = useOcgRuntime();
  const [activeSessionId, setActiveSessionId] = useState("design-pwa-shell");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const [missionMode, setMissionMode] = useState<InspectorMode>("docked");
  const [mobileMissionOpen, setMobileMissionOpen] = useState(false);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setMobileNavOpen(false);
        setMobileMissionOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const activeSession = snapshot.sessions.find((session) => session.id === activeSessionId) ?? snapshot.sessions[0];
  const activeSessionKey = activeSession?.id;
  const activeWorkType = activeSession?.workType;

  const handleNewChat = useCallback(async () => {
    if (!activeWorkType) return;
    const session = await createSession({ workType: activeWorkType });
    setActiveSessionId(session.id);
    setMobileNavOpen(false);
  }, [activeWorkType, createSession]);

  const selectSession = useCallback((id: string) => {
    setActiveSessionId(id);
    setMobileNavOpen(false);
  }, []);

  // The provider owns runtime data. This callback only adapts the presentational ChatView contract.
  const handleSendMessage = useCallback(
    (content: string) => activeSessionKey ? sendMessage(activeSessionKey, { content }) : undefined,
    [activeSessionKey, sendMessage],
  );

  if (!activeSession) return null;

  const messages = snapshot.messagesBySession[activeSession.id] ?? [];
  const mission = snapshot.missionsBySession[activeSession.id];
  const observability = snapshot.observabilityBySession[activeSession.id];
  const missionOpen = missionMode !== "collapsed";

  const sidebar = (
    <OcgSidebar
      sessions={snapshot.sessions}
      activeSessionId={activeSession.id}
      collapsed={sidebarCollapsed}
      onToggle={() => setSidebarCollapsed((value) => !value)}
      onSelect={selectSession}
      onNewChat={handleNewChat}
      runtimeStatus={snapshot.status}
    />
  );

  return (
    <div className="flex h-dvh overflow-hidden bg-background text-foreground">
      <aside
        aria-label="OCG navigation"
        className={cn(
          "hidden shrink-0 overflow-hidden border-r border-border bg-sidebar transition-[width] duration-200 ease-out lg:block",
          sidebarCollapsed ? "w-16" : "w-[272px]",
        )}
      >
        {sidebar}
      </aside>

      <div
        className={cn("fixed inset-0 z-50 lg:hidden", !mobileNavOpen && "pointer-events-none")}
        aria-hidden={!mobileNavOpen}
      >
        <div
          onClick={() => setMobileNavOpen(false)}
          className={cn(
            "absolute inset-0 bg-black/40 transition-opacity duration-200",
            mobileNavOpen ? "opacity-100" : "opacity-0",
          )}
        />
        <aside
          aria-label="OCG navigation"
          className={cn(
            "absolute inset-y-0 left-0 w-[272px] border-r border-border bg-sidebar transition-transform duration-200 ease-out",
            mobileNavOpen ? "translate-x-0" : "-translate-x-full",
          )}
        >
          <OcgSidebar
            sessions={snapshot.sessions}
            activeSessionId={activeSession.id}
            collapsed={false}
            onToggle={() => setMobileNavOpen(false)}
            onSelect={selectSession}
            onNewChat={handleNewChat}
            runtimeStatus={snapshot.status}
          />
        </aside>
      </div>

      <div className="flex min-w-0 flex-1 flex-col">
        <OcgTopbar
          session={activeSession}
          sidebarCollapsed={sidebarCollapsed}
          missionOpen={missionOpen}
          onToggleSidebar={() => setSidebarCollapsed((value) => !value)}
           onToggleMission={() => setMissionMode((value) => value === "collapsed" ? "docked" : "collapsed")}
          onOpenMobileSidebar={() => setMobileNavOpen(true)}
          onOpenMobileMission={() => setMobileMissionOpen(true)}
          runtimeStatus={snapshot.status}
        />
        <main aria-label="OCG workspace" className="flex min-h-0 flex-1">
          <div className="flex min-w-0 flex-1 flex-col">
            <ChatView
              key={activeSession.id}
              session={activeSession}
              messages={messages}
              runtimeStatus={snapshot.status}
              mission={mission}
              onSendMessage={handleSendMessage}
            />
          </div>

          <aside
            aria-label="Mission panel"
            className={cn(
              "hidden shrink-0 overflow-hidden border-border bg-background transition-[width,opacity] duration-200 ease-out lg:block",
               missionMode === "expanded" ? "w-[min(640px,42vw)] border-l opacity-100" : missionMode === "docked" ? "w-[min(360px,28vw)] border-l opacity-100" : "w-0 border-l-0 opacity-0",
            )}
          >
            <div className={cn("h-full", missionMode === "expanded" ? "w-[min(640px,42vw)]" : "w-[min(360px,28vw)]")}>
              {mission && missionOpen && <MissionView mission={mission} observability={observability} mode={missionMode} onModeChange={setMissionMode} onClose={() => setMissionMode("collapsed")} />}
            </div>
          </aside>
        </main>
      </div>

      <div
        className={cn("fixed inset-0 z-50 lg:hidden", !mobileMissionOpen && "pointer-events-none")}
        aria-hidden={!mobileMissionOpen}
      >
        <div
          onClick={() => setMobileMissionOpen(false)}
          className={cn(
            "absolute inset-0 bg-black/40 transition-opacity duration-200",
            mobileMissionOpen ? "opacity-100" : "opacity-0",
          )}
        />
        <aside
          aria-label="Mission panel"
          className={cn(
             "absolute inset-y-0 right-0 w-full max-w-none border-l border-border bg-background transition-transform duration-200 ease-out sm:w-[640px] sm:max-w-[85vw]",
             mobileMissionOpen ? "translate-x-0" : "translate-x-full",
           )}
          >
          {mobileMissionOpen && mission && <MissionView mission={mission} observability={observability} mode="expanded" onClose={() => setMobileMissionOpen(false)} />}
        </aside>
      </div>
    </div>
  );
}
