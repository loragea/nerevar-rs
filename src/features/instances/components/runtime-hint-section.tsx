import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  emptyRuntimeSource,
  normalizeRepo,
  TES3MP_REPO,
} from "@/features/instances/schemas/runtime-source-schema";
import { ReleaseSelector } from "@/features/tes3mp-releases/components/release-selector";
import type { InstanceConfig, RuntimeSource } from "@/types";
import { invoke } from "@tauri-apps/api/core";
import { Loader2, Save, X } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";

type GithubHint = Extract<RuntimeSource, { kind: "githubRelease" }>;

function draftFrom(instance: InstanceConfig): GithubHint {
  const hint = instance.runtimeHint;
  return hint && hint.kind === "githubRelease"
    ? hint
    : { ...emptyRuntimeSource };
}

/**
 * The host operator's suggestion of a TES3MP runtime for the players who
 * connect to this instance.
 *
 * It is advertised in the manifest summary and preselected in a connecting
 * player's runtime picker — nothing is pushed, and the player can pick
 * something else. Only a GitHub release can be suggested: a path on this
 * machine means nothing on theirs, which is why this control is a repository
 * and a release rather than the full runtime picker.
 */
export function RuntimeHintSection({ instance }: { instance: InstanceConfig }) {
  const [draft, setDraft] = useState<GithubHint>(() => draftFrom(instance));
  const [saving, setSaving] = useState(false);

  const hasHint = instance.runtimeHint != null;
  const repoIsUsable = normalizeRepo(draft.repo) !== null;

  const write = async (hint: GithubHint | null, message: string) => {
    setSaving(true);
    try {
      await invoke<RuntimeSource | null>("set_instance_runtime_hint", {
        instanceId: instance.id,
        hint,
      });
      toast.success(message);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="font-serif text-sm text-foreground/60">
        Suggest this runtime to players. Their connection page preselects it and
        says it came from you; they stay free to choose another.
      </p>

      <Input
        value={draft.repo}
        spellCheck={false}
        autoCapitalize="none"
        autoCorrect="off"
        placeholder={TES3MP_REPO}
        aria-label="Suggested GitHub repository"
        disabled={saving}
        className="font-mono text-xs"
        onChange={(event) =>
          setDraft({
            ...draft,
            repo: event.target.value,
            releaseId: "",
            tag: "",
          })
        }
        onBlur={(event) => {
          const normalized = normalizeRepo(event.target.value);
          if (normalized !== null && normalized !== draft.repo) {
            setDraft({ ...draft, repo: normalized, releaseId: "", tag: "" });
          }
        }}
      />

      <ReleaseSelector
        value={draft}
        onValueChange={(next) => {
          if (next.kind === "githubRelease") setDraft(next);
        }}
        repo={draft.repo}
        disabled={saving}
      />

      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          className="h-10 flex-1 font-display tracking-[0.15em] uppercase"
          disabled={saving || !repoIsUsable}
          onClick={() =>
            void write(
              { ...draft, repo: normalizeRepo(draft.repo) ?? draft.repo },
              "Runtime suggestion saved",
            )
          }
        >
          {saving ? (
            <Loader2 className="animate-spin" data-icon="inline-start" />
          ) : (
            <Save data-icon="inline-start" />
          )}
          Save suggestion
        </Button>
        <Button
          type="button"
          variant="secondary"
          className="h-10 font-display tracking-[0.15em] uppercase"
          disabled={saving || !hasHint}
          onClick={() => {
            setDraft({ ...emptyRuntimeSource });
            void write(null, "Runtime suggestion removed");
          }}
        >
          <X data-icon="inline-start" />
          Remove
        </Button>
      </div>
    </div>
  );
}
