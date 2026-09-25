import { OcgEntryGate } from "@/components/ocg/bootstrap/entry-gate";
import type { WorkspaceView } from "@/components/ocg/layout/app-shell";
import { resolveControlCenterView } from "@/components/ocg/control-center/domain";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function Home({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const value = typeof params.scenario === "string" ? params.scenario : undefined;
  const scenario = resolveScenario(value);

  // `profiles-models` opens the Control Center by default; `view` pins a tab.
  const view: WorkspaceView =
    scenario === "profiles-models" ? "control-center" : scenario === "resource-ledger" ? "ledger" : scenario === "mission-control" ? "mission-control" : "chat";
  const controlCenterView = resolveControlCenterView(
    typeof params.view === "string" ? params.view : undefined,
  );

  return <OcgEntryGate scenario={scenario} view={view} controlCenterView={controlCenterView} />;
}
