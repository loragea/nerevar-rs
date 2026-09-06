/*
This component is responsible for managing the onboarding process.

It first asks which of the two setups the user is here for, then runs that
path:

- joining a friend's server: find Morrowind, default the data directory, a
  short guide, then the host address — which creates the instance, marks
  onboarding complete, and lands on that server's page
- hosting: the original flow (data directory, Morrowind, sync port, guide),
  which ends by marking onboarding complete

*/

import { NerevarBackgroundShell } from "@/components/custom/nerevar-background-shell";
import { useConfig } from "@/features/config/context/config-context-provider";
import { JoinFlow } from "@/features/onboarding/components/join-flow";
import {
  OnboardingFlow,
  type OnboardingStage,
} from "@/features/onboarding/components/onboarding-flow";
import {
  OnboardingPathChoice,
  type OnboardingPath,
} from "@/features/onboarding/components/onboarding-path-choice";
import { useState } from "react";

export function OnboardingManager({
  children,
  onComplete,
}: {
  children: React.ReactNode;
  onComplete: () => void;
}) {
  const config = useConfig();
  const [path, setPath] = useState<OnboardingPath | null>(null);
  const [stage, setStage] = useState<OnboardingStage>("select-data-dir");

  if (!config) {
    return (
      <NerevarBackgroundShell className="flex h-full min-h-full flex-col items-center justify-center">
        <p className="font-display text-xs tracking-[0.25em] text-foreground/60 uppercase animate-pulse">
          Loading...
        </p>
      </NerevarBackgroundShell>
    );
  }

  if (
    config.onboardingComplete &&
    config.rootPath &&
    config.rootPath.length > 0
  ) {
    return (
      <NerevarBackgroundShell className="min-h-full">
        {children}
      </NerevarBackgroundShell>
    );
  }

  return (
    <NerevarBackgroundShell className="h-full min-h-full">
      {path === null ? (
        <OnboardingPathChoice onChoose={setPath} />
      ) : path === "join" ? (
        <JoinFlow onFinish={onComplete} />
      ) : (
        <OnboardingFlow
          stage={stage}
          onStageChange={setStage}
          onFinish={onComplete}
          onLeavePath={() => setPath(null)}
        />
      )}
    </NerevarBackgroundShell>
  );
}
