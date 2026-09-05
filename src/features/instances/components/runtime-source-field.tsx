import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import {
  emptySourceOfKind,
  normalizeRepo,
  TES3MP_REPO,
  type RuntimeSourceKind,
} from "@/features/instances/schemas/runtime-source-schema";
import { ReleaseSelector } from "@/features/tes3mp-releases/components/release-selector";
import type { RuntimeInspection, RuntimeSource } from "@/types";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, CheckCircle2, Loader2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";

const KIND_LABELS: Record<RuntimeSourceKind, string> = {
  githubRelease: "GitHub release",
  localDirectory: "Local directory",
  archive: "Archive file",
};

const KIND_HINTS: Record<RuntimeSourceKind, string> = {
  githubRelease: "",
  localDirectory:
    "A TES3MP install you already have unpacked — an extracted release, or a fork build. It is copied into this instance, not run in place.",
  archive:
    "A TES3MP release archive on disk (.zip, .tar.gz or .tgz). It is extracted into this instance.",
};

const PICKER_COMMANDS: Record<RuntimeSourceKind, string> = {
  githubRelease: "",
  localDirectory: "pick_runtime_directory",
  archive: "pick_runtime_archive",
};

type InspectionState =
  | { status: "idle" }
  | { status: "checking" }
  | { status: "ok"; inspection: RuntimeInspection }
  | { status: "error"; message: string };

/**
 * Picks where an instance's TES3MP runtime comes from.
 *
 * The `githubRelease` branch is the default: a repository (the official one
 * unless the user names a fork's) and the release dropdown listing that
 * repository's releases. The two local branches add a path field, a native
 * picker, and an inline inspection of what was picked.
 *
 * `onBlockingChange` reports whether the current pick would fail the create:
 * a local source whose inspection lists missing pieces is not a TES3MP
 * runtime, and the backend would reject it after the copy. The form disables
 * its submit instead, so the user finds out on the pick.
 */
export function RuntimeSourceField({
  value,
  onValueChange,
  disabled,
  onBlockingChange,
}: {
  value: RuntimeSource;
  onValueChange: (value: RuntimeSource) => void;
  disabled?: boolean;
  onBlockingChange?: (blocked: boolean) => void;
}) {
  const [inspection, setInspection] = useState<InspectionState>({
    status: "idle",
  });
  const path = value.kind === "githubRelease" ? "" : value.path;
  // Only the newest inspection may write state: the native picker can be
  // reopened while a scan of the previous pick is still running.
  const requestId = useRef(0);

  useEffect(() => {
    if (value.kind === "githubRelease" || path.length === 0) {
      setInspection({ status: "idle" });
      return;
    }

    const request = ++requestId.current;
    setInspection({ status: "checking" });
    invoke<RuntimeInspection>("inspect_runtime_source", {
      source: { kind: value.kind, path },
    })
      .then((result) => {
        if (requestId.current === request) {
          setInspection({ status: "ok", inspection: result });
        }
      })
      .catch((error) => {
        if (requestId.current === request) {
          setInspection({ status: "error", message: String(error) });
        }
      });
  }, [value.kind, path]);

  const repoIsUsable =
    value.kind === "githubRelease" && normalizeRepo(value.repo) !== null;

  const blocked =
    inspection.status === "checking" ||
    inspection.status === "error" ||
    (inspection.status === "ok" && inspection.inspection.missing.length > 0);

  useEffect(() => {
    onBlockingChange?.(blocked);
  }, [blocked, onBlockingChange]);

  const handleKindChange = (next: string) => {
    // Radix clears the value when the pressed item is pressed again; a
    // segmented choice always has exactly one answer.
    if (!next || next === value.kind) return;
    onValueChange(emptySourceOfKind(next as RuntimeSourceKind));
  };

  // A repository change invalidates the release: an id from one repo names
  // nothing in another. The field keeps what was typed (so the user can
  // finish typing an incomplete name) and normalises it on blur.
  const handleRepoChange = (repo: string) => {
    if (value.kind !== "githubRelease") return;
    onValueChange({ ...value, repo, releaseId: "", tag: "" });
  };

  const handleRepoBlur = (repo: string) => {
    if (value.kind !== "githubRelease") return;
    const normalized = normalizeRepo(repo);
    if (normalized !== null && normalized !== repo) {
      onValueChange({ ...value, repo: normalized, releaseId: "", tag: "" });
    }
  };

  const browse = () => {
    if (value.kind === "githubRelease") return;
    const kind = value.kind;
    void invoke<string>(PICKER_COMMANDS[kind])
      .then((picked) => onValueChange({ kind, path: picked }))
      .catch(() => {
        // A cancelled dialog is not an error worth reporting.
      });
  };

  return (
    <div className="flex flex-col gap-3">
      <ToggleGroup
        type="single"
        variant="outline"
        spacing={0}
        value={value.kind}
        onValueChange={handleKindChange}
        disabled={disabled}
        aria-label="Runtime source"
      >
        {(Object.keys(KIND_LABELS) as RuntimeSourceKind[]).map((kind) => (
          <ToggleGroupItem key={kind} value={kind} className="px-3">
            {KIND_LABELS[kind]}
          </ToggleGroupItem>
        ))}
      </ToggleGroup>

      {value.kind === "githubRelease" ? (
        <>
          <div className="flex flex-col gap-1">
            <Input
              value={value.repo}
              spellCheck={false}
              autoCapitalize="none"
              autoCorrect="off"
              placeholder={TES3MP_REPO}
              aria-label="GitHub repository"
              disabled={disabled}
              className="font-mono text-xs"
              onChange={(event) => handleRepoChange(event.target.value)}
              onBlur={(event) => handleRepoBlur(event.target.value)}
            />
            <p className="text-left font-sans text-xs leading-loose font-light tracking-[0.1em] text-foreground/75">
              {repoIsUsable
                ? `Releases published by ${normalizeRepo(value.repo)}. The official ${TES3MP_REPO} is the default; a fork that publishes its own builds goes here.`
                : "Enter the repository as owner/name (a pasted github.com link works too)."}
            </p>
          </div>
          <ReleaseSelector
            value={value}
            onValueChange={onValueChange}
            repo={value.repo}
            disabled={disabled}
          />
        </>
      ) : (
        <>
          <p className="text-left font-sans text-xs leading-loose font-light tracking-[0.1em] text-foreground/75">
            {KIND_HINTS[value.kind]}
          </p>
          <div className="flex gap-2">
            <Input
              readOnly
              value={path}
              placeholder={
                value.kind === "localDirectory"
                  ? "No folder selected"
                  : "No archive selected"
              }
              className="bg-input/40 font-mono text-xs"
            />
            <Button
              type="button"
              variant="outline"
              className="shrink-0 font-display text-[0.75rem] tracking-[0.3em] uppercase"
              onClick={browse}
              disabled={disabled}
            >
              Browse
            </Button>
          </div>
          <InspectionSummary state={inspection} />
        </>
      )}
    </div>
  );
}

function InspectionSummary({ state }: { state: InspectionState }) {
  if (state.status === "idle") {
    return null;
  }

  if (state.status === "checking") {
    return (
      <p className="flex items-center gap-2 text-left font-mono text-xs text-foreground/60">
        <Loader2 className="size-3.5 shrink-0 animate-spin" />
        Checking the TES3MP install…
      </p>
    );
  }

  if (state.status === "error") {
    return (
      <p className="flex items-start gap-2 text-left font-mono text-xs text-destructive">
        <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
        {state.message}
      </p>
    );
  }

  const { inspection } = state;
  const roles = [
    inspection.hasClientExe ? "client" : null,
    inspection.hasServerExe ? "server" : null,
  ]
    .filter(Boolean)
    .join(" + ");

  if (inspection.missing.length > 0) {
    return (
      <div className="flex items-start gap-2 text-left font-mono text-xs text-destructive">
        <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
        <span>
          Not a usable TES3MP runtime — missing {inspection.missing.join(", ")}.
        </span>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-1 text-left font-mono text-xs text-foreground/70">
      <p className="flex items-start gap-2">
        <CheckCircle2 className="mt-0.5 size-3.5 shrink-0 text-emerald-500" />
        <span>
          TES3MP runtime — OpenMW {inspection.versionDisplay}
          {roles ? ` · ${roles}` : ""}
        </span>
      </p>
      {inspection.warnings.map((warning) => (
        <p key={warning} className="pl-5 text-foreground/55">
          {warning}
        </p>
      ))}
    </div>
  );
}
