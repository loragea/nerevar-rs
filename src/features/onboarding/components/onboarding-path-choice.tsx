import { NerevarHeader } from "@/components/custom/nerevar-header";
import {
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { OnboardingStepCard } from "@/features/onboarding/components/onboarding-step-shell";
import { ChevronRight, Play, Server } from "lucide-react";

/** Which of the two setups the player is here for. */
export type OnboardingPath = "join" | "host";

const CHOICES: {
  id: OnboardingPath;
  icon: typeof Play;
  title: string;
  description: string;
}[] = [
  {
    id: "join",
    icon: Play,
    title: "I'm joining a friend's server",
    description:
      "Paste the address they gave you. Nerevar finds your Morrowind, downloads the server's mods, and keeps them up to date.",
  },
  {
    id: "host",
    icon: Server,
    title: "I'm hosting",
    description:
      "Set up a server of your own: pick the mods, host the manifest, and share the address with your players.",
  },
];

/**
 * The first onboarding screen. Everything after it differs between the two
 * paths — the joining path asks for almost nothing, the hosting path is the
 * setup Nerevar has always had — so the choice is made before any of it.
 */
export function OnboardingPathChoice({
  onChoose,
}: {
  onChoose: (path: OnboardingPath) => void;
}) {
  return (
    <div className="flex h-full w-full flex-col items-center justify-center px-4 py-10">
      <NerevarHeader title="NEREVAR" subtitle="Setup" />

      <div className="mt-8 w-full max-w-lg">
        <OnboardingStepCard>
          <CardHeader className="border-b border-border/50 px-6 pb-4 pt-6">
            <CardTitle className="font-display text-base font-bold tracking-[0.2em] text-accent uppercase">
              How will you play?
            </CardTitle>
            <CardDescription className="font-serif text-base font-light tracking-[0.05em] leading-relaxed text-foreground/75">
              You can do both later — this only decides what Nerevar sets up
              now.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-3 px-6 py-5">
            {CHOICES.map((choice) => (
              <button
                key={choice.id}
                type="button"
                onClick={() => onChoose(choice.id)}
                className="flex w-full items-center gap-4 rounded-lg border border-border/50 bg-background/30 px-4 py-4 text-left transition-colors hover:border-accent/50 hover:bg-accent/5"
              >
                <span className="flex size-10 shrink-0 items-center justify-center rounded-lg border border-accent/40 bg-accent/10 text-accent">
                  <choice.icon className="size-5" />
                </span>
                <span className="flex-1">
                  <span className="block font-display text-sm tracking-[0.08em] text-foreground">
                    {choice.title}
                  </span>
                  <span className="mt-1 block font-serif text-sm leading-relaxed text-foreground/70">
                    {choice.description}
                  </span>
                </span>
                <ChevronRight className="size-4 shrink-0 text-accent/70" />
              </button>
            ))}
          </CardContent>
        </OnboardingStepCard>
      </div>
    </div>
  );
}
