import { OcgEntryGate } from "@/components/ocg/bootstrap/entry-gate";
import { resolveWorkspaceView } from "@/components/ocg/layout/view-domain";
import { resolveControlCenterView } from "@/components/ocg/control-center/domain";
import { resolveProjectParam } from "@/components/ocg/project/domain";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function Home({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const value = typeof params.scenario === "string" ? params.scenario : undefined;
  const scenario = resolveScenario(value);

  // An explicit view preserves the current scenario/runtime instance. When no
  // view is given, the existing scenario-derived defaults still apply.
  const requestedView = typeof params.view === "string" ? params.view : undefined;
  const view = resolveWorkspaceView(scenario, requestedView);
  const controlCenterView = resolveControlCenterView(requestedView);
  const initialProjectId = resolveProjectParam(params.project);

  return (
    <OcgEntryGate
      scenario={scenario}
      view={view}
      controlCenterView={controlCenterView}
      initialProjectId={initialProjectId}
    />
  );
}
