import { OcgEntryGate } from "@/components/ocg/bootstrap/entry-gate";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function ResourceLedgerPage({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const value = typeof params.scenario === "string" ? params.scenario : undefined;
  // The route defaults to the resource-ledger scenario so a plain bookmark works.
  // An explicit scenario still resolves through the shared entry gate.
  return <OcgEntryGate scenario={value ? resolveScenario(value) : "resource-ledger"} view="ledger" />;
}
