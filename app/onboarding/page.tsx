import { OcgEntryGate } from "@/components/ocg/bootstrap/entry-gate";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function OnboardingPage({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const value = typeof params.scenario === "string" ? params.scenario : undefined;
  return <OcgEntryGate scenario={value ? resolveScenario(value) : "local-first-run"} />;
}
