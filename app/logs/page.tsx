import { OcgEntryGate } from "@/components/ocg/bootstrap/entry-gate";
import { resolveProjectParam } from "@/components/ocg/project/domain";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function LogsPage({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const scenario = resolveScenario(typeof params.scenario === "string" ? params.scenario : "logs-live");
  return (
    <OcgEntryGate
      scenario={scenario}
      view="logs"
      initialProjectId={resolveProjectParam(params.project)}
    />
  );
}
