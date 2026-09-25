import { OcgEntryGate } from "@/components/ocg/bootstrap/entry-gate";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function SettingsPage({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const scenario = resolveScenario(typeof params.scenario === "string" ? params.scenario : "local-ready");
  return <OcgEntryGate scenario={scenario} view="settings" />;
}
