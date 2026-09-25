"use client";

import { useCallback, useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { cn } from "@/lib/utils";
import { ChatView } from "../chat/chat-view";
import { MissionView } from "../mission/mission-view";
import { OcgSidebar } from "../sidebar/ocg-sidebar";
import { OcgTopbar } from "../topbar/ocg-topbar";
import { ResourceLedgerSurface } from "../resource-ledger/resource-ledger-surface";
import { ControlCenterSurface } from "../control-center/control-center-surface";
import { MissionControlSurface } from "../mission-control/mission-control-surface";
import { LogsSurface } from "../logs/logs-surface";
import { SettingsSurface } from "../settings/settings-surface";
import type { ControlCenterView } from "../control-center/domain";
import { OcgRuntimeProvider, useOcgRuntime } from "../runtime/runtime-context";
import type { ScenarioId } from "../runtime/runtime-types";
import type { InspectorMode } from "../observability/inspector-state";

export type WorkspaceView = "chat" | "ledger" | "control-center" | "mission-control" | "logs" | "settings";

export function AppShell({
  scenario,
  view = "chat",
  controlCenterView = "profiles",
}: {
  scenario: ScenarioId;
  view?: WorkspaceView;
  controlCenterView?: ControlCenterView;
}) {
  return (
    <OcgRuntimeProvider scenario={scenario}>
      <RuntimeWorkspace view={view} controlCenterView={controlCenterView} />
    </OcgRuntimeProvider>
  );
}

export function RuntimeWorkspace({
  view = "chat",
  controlCenterView = "profiles",
}: {
  view?: WorkspaceView;
  controlCenterView?: ControlCenterView;
}) {
  const { snapshot, createSession, sendMessage, setActiveProfile } = useOcgRuntime();
  const router = useRouter();
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
  const isLedger = view === "ledger";
  const isControlCenter = view === "control-center";
  const isMissionControl = view === "mission-control";
  const isLogs = view === "logs";
  const isSettings = view === "settings";

  const handleNewChat = useCallback(async () => {
    if (!activeWorkType) return;
    const session = await createSession({ workType: activeWorkType });
    setActiveSessionId(session.id);
    setMobileNavOpen(false);
  }, [activeWorkType, createSession]);

  const selectSession = useCallback((id: string) => {
    setActiveSessionId(id);
    setMobileNavOpen(false);
    // Session selection is a chat action; leave other workspace views.
    if (view !== "chat") router.push("/");
  }, [router, view]);

  // The provider owns runtime data. This callback only adapts the presentational ChatView contract.
  const handleSendMessage = useCallback(
    (content: string) => activeSessionKey ? sendMessage(activeSessionKey, { content }) : undefined,
    [activeSessionKey, sendMessage],
  );

  const handleOpenChat = useCallback(() => {
    setMobileNavOpen(false);
    setMobileMissionOpen(false);
    if (view !== "chat") router.push("/");
  }, [router, view]);

  const handleOpenLedger = useCallback(() => {
    setMobileNavOpen(false);
    setMobileMissionOpen(false);
    router.push(view === "ledger" ? "/" : "/resource-ledger");
  }, [router, view]);

  const handleOpenControlCenter = useCallback(() => {
    setMobileNavOpen(false);
    setMobileMissionOpen(false);
    router.push(view === "control-center" ? "/" : "/?scenario=profiles-models");
  }, [router, view]);

  const handleOpenMissionControl = useCallback(() => {
    setMobileNavOpen(false);
    setMobileMissionOpen(false);
    router.push(isMissionControl ? "/" : "/?scenario=mission-control");
  }, [isMissionControl, router]);

  const handleOpenLogs = useCallback(() => {
    setMobileNavOpen(false);
    setMobileMissionOpen(false);
    router.push(isLogs ? "/" : "/?scenario=logs-live");
  }, [isLogs, router]);

  const handleOpenSettings = useCallback(() => {
    setMobileNavOpen(false);
    setMobileMissionOpen(false);
    router.push(isSettings ? "/" : "/settings");
  }, [isSettings, router]);

  const handleSelectProfile = useCallback((profileId: string) => {
    void setActiveProfile(profileId);
  }, [setActiveProfile]);

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
      activeWorkspace={view}
      onOpenChat={handleOpenChat}
      onOpenLedger={handleOpenLedger}
      onOpenControlCenter={handleOpenControlCenter}
      onOpenMissionControl={handleOpenMissionControl}
      onOpenLogs={handleOpenLogs}
      onOpenSettings={handleOpenSettings}
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
            activeWorkspace={view}
            onOpenChat={handleOpenChat}
            onOpenLedger={handleOpenLedger}
            onOpenControlCenter={handleOpenControlCenter}
            onOpenMissionControl={handleOpenMissionControl}
            onOpenLogs={handleOpenLogs}
            onOpenSettings={handleOpenSettings}
          />
        </aside>
      </div>

      <div className="flex min-w-0 flex-1 flex-col">
        <OcgTopbar
          session={activeSession}
          sidebarCollapsed={sidebarCollapsed}
          missionOpen={missionOpen}
          missionControls={!isLedger && !isControlCenter && !isMissionControl && !isLogs && !isSettings}
          ledgerActive={isLedger}
          controlCenterActive={isControlCenter}
          missionControlActive={isMissionControl}
          logsActive={isLogs}
          settingsActive={isSettings}
          onToggleSidebar={() => setSidebarCollapsed((value) => !value)}
          onToggleMission={() => setMissionMode((value) => value === "collapsed" ? "docked" : "collapsed")}
          onOpenMobileSidebar={() => setMobileNavOpen(true)}
          onOpenMobileMission={() => setMobileMissionOpen(true)}
          onOpenLedger={handleOpenLedger}
          onOpenControlCenter={handleOpenControlCenter}
          onOpenMissionControl={handleOpenMissionControl}
          onOpenLogs={handleOpenLogs}
          onOpenSettings={handleOpenSettings}
          runtimeStatus={snapshot.status}
        />
        {isLedger ? (
          <main aria-label="Resource ledger" className="flex min-h-0 flex-1 overflow-hidden">
            <ResourceLedgerSurface ledger={snapshot.resourceLedger} />
          </main>
        ) : isControlCenter ? (
          <main aria-label="Control Center" className="flex min-h-0 flex-1 overflow-hidden">
            <ControlCenterSurface
              key={controlCenterView}
              bootstrap={snapshot.bootstrap}
              ledger={snapshot.resourceLedger}
              initialView={controlCenterView}
              onSelectProfile={handleSelectProfile}
            />
          </main>
        ) : isMissionControl ? (
          <main aria-label="Mission Control" className="flex min-h-0 flex-1 overflow-hidden">
            {snapshot.executionBySession[activeSession.id] ? (
              <MissionControlSurface
                execution={snapshot.executionBySession[activeSession.id]!}
                onOpenInspector={() => router.push("/")}
              />
            ) : (
              <div className="flex flex-1 items-center justify-center p-6 text-[12px] text-muted-foreground">No Mission execution is available.</div>
            )}
          </main>
        ) : isLogs ? (
          <main aria-label="Logs and diagnostics" className="flex min-h-0 flex-1 overflow-hidden">
            <LogsSurface key={snapshot.scenario} snapshot={snapshot} />
          </main>
        ) : isSettings ? (
          <main aria-label="Settings" className="flex min-h-0 flex-1 overflow-hidden">
            <SettingsSurface snapshot={snapshot} />
          </main>
        ) : (
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
                {mission && missionOpen && <MissionView mission={mission} observability={observability} mode={missionMode} onModeChange={setMissionMode} onClose={() => setMissionMode("collapsed")} onOpenMissionControl={handleOpenMissionControl} />}
              </div>
            </aside>
          </main>
        )}
      </div>

      {!isLedger && !isControlCenter && !isMissionControl && !isLogs && !isSettings && (
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
            {mobileMissionOpen && mission && <MissionView mission={mission} observability={observability} mode="expanded" onClose={() => setMobileMissionOpen(false)} onOpenMissionControl={handleOpenMissionControl} />}
          </aside>
        </div>
      )}
    </div>
  );
}
