import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { GithubReleaseResponse, RuntimeSource } from "@/types";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectItemText,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Badge } from "@/components/ui/badge";
import {
  normalizeRepo,
  TES3MP_REPO,
} from "@/features/instances/schemas/runtime-source-schema";
import { cn } from "@/lib/utils";

const RECOMMENDED_RELEASE = "TES3MP 0.8.1";

function RecommendedBadge({ className }: { className?: string }) {
  return (
    <Badge
      variant="outline"
      className={cn(
        "ml-auto shrink-0 border-accent/40 bg-gradient-to-r from-accent/80 to-accent/40 px-2 font-bold tracking-[0.08em] text-gradient-gold glow-gold",
        className,
      )}
    >
      Recommended
    </Badge>
  );
}

function UnsupportedBadge({ className }: { className?: string }) {
  return (
    <Badge
      variant="outline"
      className={cn(
        "ml-auto shrink-0 border-border/60 bg-muted/40 px-2 font-bold tracking-[0.08em] text-foreground/60",
        className,
      )}
    >
      Unsupported
    </Badge>
  );
}

/**
 * Picks a TES3MP release from `repo` and hands back the `RuntimeSource` that
 * installs it.
 *
 * `assetName` is deliberately left empty: which asset of a release is this
 * platform's runtime is a backend rule (`runtime::select_tes3mp_asset`), and
 * duplicating it here would be a second copy to keep in step.
 *
 * The recommended/unsupported badges only appear for the official
 * repository. They say which release *Nerevar* was validated against; a
 * fork's own releases are not ours to grade, and marking every one of them
 * "Unsupported" would be noise, not information.
 */
export function ReleaseSelector({
  value,
  onValueChange,
  repo = TES3MP_REPO,
  disabled,
}: {
  value: RuntimeSource;
  onValueChange: (value: RuntimeSource) => void;
  repo?: string;
  disabled?: boolean;
}) {
  const [releases, setReleases] = useState<GithubReleaseResponse[]>([]);
  const [error, setError] = useState<string | null>(null);

  const normalizedRepo = normalizeRepo(repo);
  const isOfficialRepo = normalizedRepo === TES3MP_REPO;

  const selectedId = value.kind === "githubRelease" ? value.releaseId : "";
  const selectedRelease = releases.find(
    (release) => release.id.toString() === selectedId,
  );
  const showRecommendedBadge =
    isOfficialRepo && selectedRelease?.name === RECOMMENDED_RELEASE;
  const showUnsupportedBadge =
    isOfficialRepo && selectedRelease !== undefined && !showRecommendedBadge;

  useEffect(() => {
    if (normalizedRepo === null) {
      setReleases([]);
      setError(null);
      return;
    }

    // Only the newest listing may land: the repo field is typed into, so a
    // slower request for a half-typed repository must not overwrite a
    // finished one.
    let current = true;
    setError(null);
    invoke<GithubReleaseResponse[]>("get_all_releases", {
      repo: normalizedRepo,
    })
      .then((fetched) => {
        if (!current) return;
        setReleases(fetched || []);
      })
      .catch((failure) => {
        if (!current) return;
        setReleases([]);
        setError(String(failure));
      });
    return () => {
      current = false;
    };
  }, [normalizedRepo]);

  const handleReleaseChange = (nextValue: string) => {
    const release = releases.find((r) => r.id.toString() === nextValue);
    onValueChange({
      kind: "githubRelease",
      repo: normalizedRepo ?? repo,
      releaseId: nextValue,
      tag: release?.tag_name ?? "",
      assetName: "",
    });
  };

  if (normalizedRepo === null || error !== null || releases.length === 0) {
    return (
      <div className="flex flex-col gap-2">
        <Select disabled>
          <SelectTrigger className="min-w-full">
            <SelectValue
              placeholder={
                normalizedRepo === null
                  ? "Enter a repository as owner/name"
                  : error !== null
                    ? "No releases"
                    : "Loading..."
              }
              className={cn(
                "text-foreground/50",
                normalizedRepo !== null && error === null && "animate-pulse",
              )}
            />
          </SelectTrigger>
        </Select>
        {error !== null ? (
          <p className="text-left font-mono text-xs text-destructive">
            {error}
          </p>
        ) : null}
      </div>
    );
  }

  return (
    <Select
      value={selectedId}
      onValueChange={handleReleaseChange}
      disabled={disabled}
    >
      <SelectTrigger className="flex w-full min-w-full">
        <span className="flex min-w-0 flex-1 items-center justify-between gap-2 pr-1">
          <SelectValue placeholder="Select a release" className="truncate" />
          {showRecommendedBadge && <RecommendedBadge />}
          {showUnsupportedBadge && <UnsupportedBadge />}
        </span>
      </SelectTrigger>
      <SelectContent
        position="popper"
        className="max-h-[350px] w-[var(--radix-select-trigger-width)]"
      >
        {releases.map((release) => (
          <SelectItem
            key={release.id}
            value={release.id.toString()}
            textValue={release.name || release.tag_name}
            className="my-1 justify-between rounded-lg border border-border/50 p-2"
          >
            <SelectItemText>{release.name || release.tag_name}</SelectItemText>
            {isOfficialRepo ? (
              release.name === RECOMMENDED_RELEASE ? (
                <RecommendedBadge className="mr-8" />
              ) : (
                <UnsupportedBadge className="mr-8" />
              )
            ) : null}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
