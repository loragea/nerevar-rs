import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { ArrowLeft, ArrowRight, Check } from "lucide-react";
import { motion } from "motion/react";

export const EASE = [0.22, 1, 0.36, 1] as const;

/** The step-to-step transition shared by both onboarding paths. */
export const slideVariants = {
  enter: (direction: number) => ({
    opacity: 0,
    x: direction > 0 ? 28 : -28,
    filter: "blur(4px)",
  }),
  center: {
    opacity: 1,
    x: 0,
    filter: "blur(0px)",
  },
  exit: (direction: number) => ({
    opacity: 0,
    x: direction > 0 ? -20 : 20,
    filter: "blur(4px)",
  }),
};

/** One dot per step of a flow, with the current one lit. */
export function OnboardingProgress({
  steps,
  currentIndex,
}: {
  steps: { id: string; label: string }[];
  currentIndex: number;
}) {
  return (
    <ol className="flex items-center gap-2 sm:gap-3">
      {steps.map((step, index) => {
        const isComplete = index < currentIndex;
        const isCurrent = index === currentIndex;

        return (
          <li key={step.id} className="flex items-center gap-2 sm:gap-3">
            <motion.div
              layout
              className={cn(
                "flex items-center gap-2 rounded-full border px-2.5 py-1 transition-colors sm:px-3",
                isCurrent &&
                  "border-accent/50 bg-card/80 shadow-[0_0_12px_hsl(var(--accent)/0.15)]",
                isComplete && "border-accent/30 bg-card/50",
                !isCurrent && !isComplete && "border-border/50 bg-card/30",
              )}
              transition={{ duration: 0.25, ease: EASE }}
            >
              <span
                className={cn(
                  "flex size-5 items-center justify-center rounded-full text-[0.75rem] font-display font-semibold",
                  isCurrent && "bg-accent text-accent-foreground",
                  isComplete && "bg-accent/80 text-accent-foreground",
                  !isCurrent && !isComplete && "bg-muted text-muted-foreground",
                )}
              >
                {isComplete ? <Check className="size-3" /> : index + 1}
              </span>
              <span
                className={cn(
                  "hidden font-display text-[0.75rem] tracking-[0.3em] uppercase sm:inline",
                  isCurrent ? "text-accent font-bold" : "text-foreground/55",
                )}
              >
                {step.label}
              </span>
            </motion.div>
            {index < steps.length - 1 && (
              <div
                className={cn(
                  "h-px w-4 sm:w-6",
                  index < currentIndex ? "bg-accent/50" : "bg-border/60",
                )}
              />
            )}
          </li>
        );
      })}
    </ol>
  );
}

export function OnboardingStepCard({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <Card
      className={cn(
        "gap-0 border-border/80 bg-card/75 py-0 shadow-[0_0_20px_hsl(var(--accent)/0.08)] ring-accent/15 backdrop-blur-sm",
        className,
      )}
    >
      {children}
    </Card>
  );
}

export function StepActions({
  onBack,
  onPrimary,
  primaryLabel,
  showBack = true,
  nextDisabled = false,
}: {
  onBack?: () => void;
  onPrimary: () => void;
  primaryLabel: string;
  showBack?: boolean;
  nextDisabled?: boolean;
}) {
  return (
    <div className="flex items-center justify-between gap-3 border-t border-border/50 px-6 py-4">
      {showBack && onBack ? (
        <Button
          variant="outline"
          size="sm"
          onClick={onBack}
          className="text-foreground/70"
        >
          <ArrowLeft data-icon="inline-start" />
          Back
        </Button>
      ) : (
        <span />
      )}
      <Button
        variant="launch"
        size="sm"
        onClick={onPrimary}
        disabled={nextDisabled}
      >
        {primaryLabel}
        <ArrowRight data-icon="inline-end" />
      </Button>
    </div>
  );
}
