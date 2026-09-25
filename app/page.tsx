import { AppShell } from "@/components/ocg/layout/app-shell";
import { resolveScenario } from "@/components/ocg/runtime/scenarios";

export default async function Home({
  searchParams,
}: {
  searchParams: Promise<{ [key: string]: string | string[] | undefined }>;
}) {
  const params = await searchParams;
  const value = typeof params.scenario === "string" ? params.scenario : undefined;
  return <AppShell scenario={resolveScenario(value)} />;
}
