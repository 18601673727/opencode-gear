"use client";

import { OcgRuntimeProvider, useOcgRuntime } from "../runtime/runtime-context";
import type { ScenarioId } from "../runtime/runtime-types";
import { RuntimeWorkspace } from "../layout/app-shell";
import { LoginView } from "../login/login-view";
import { OnboardingWizard } from "../onboarding/onboarding-wizard";
import { selectBootstrapEntry } from "./selectors";

/**
 * Clean root entry gate. The normalized bootstrap state decides whether the
 * operator sees the workspace, the access surface, or the setup wizard. Local
 * scenarios always resolve to the workspace, so login is never shown for them.
 */
export function OcgEntryGate({ scenario }: { scenario: ScenarioId }) {
  return (
    <OcgRuntimeProvider scenario={scenario}>
      <BootstrapSurface />
    </OcgRuntimeProvider>
  );
}

function BootstrapSurface() {
  const { snapshot } = useOcgRuntime();
  const entry = selectBootstrapEntry(snapshot.bootstrap);

  if (entry === "login") return <LoginView />;
  if (entry === "onboarding") return <OnboardingWizard />;
  return <RuntimeWorkspace />;
}
