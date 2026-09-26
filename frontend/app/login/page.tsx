import { OcgEntryGate } from "@/components/ocg/bootstrap/entry-gate";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function LoginPage({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const value = typeof params.scenario === "string" ? params.scenario : undefined;
  // The route is a bookmark for remote access. An explicit local scenario still
  // resolves to the workspace through the shared entry gate.
  return <OcgEntryGate scenario={value ? resolveScenario(value) : "remote-unauthenticated"} />;
}
