import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  assetNameError,
  emptyRuntimeSource,
  normalizeRepo,
  TES3MP_REPO,
} from "@/features/instances/schemas/runtime-source-schema";
import { ReleaseSelector } from "@/features/tes3mp-releases/components/release-selector";
import type {
  InstanceConfig,
  PlatformAssets,
  RuntimeHint,
  RuntimeSource,
} from "@/types";
import { invoke } from "@tauri-apps/api/core";
import { Loader2, Save, Settings2, X } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";

type GithubSource = Extract<RuntimeSource, { kind: "githubRelease" }>;

/** The three asset fields, as text — blank meaning "use the naming rules". */
type AssetDraft = { windows: string; linux: string; macos: string };

const ASSET_FIELDS = [
  {
    key: "windows",
    label: "Windows",
    placeholder: "tes3mp.Win64.release.0.8.1.zip",
  },
  {
    key: "linux",
    label: "Linux",
    placeholder: "tes3mp-GNU+Linux-x86_64-release-0.8.1.tar.gz",
  },
  { key: "macos", label: "macOS", placeholder: "the macOS build's file name" },
] as const;

const EMPTY_ASSETS: AssetDraft = { windows: "", linux: "", macos: "" };

function sourceFrom(instance: InstanceConfig): GithubSource {
  const hint = instance.runtimeHint;
  return hint && hint.kind === "githubRelease"
    ? {
        kind: "githubRelease",
        repo: hint.repo,
        releaseId: hint.releaseId,
        tag: hint.tag,
        assetName: hint.assetName,
      }
    : { ...emptyRuntimeSource };
}

function assetsFrom(instance: InstanceConfig): AssetDraft {
  const named = instance.runtimeHint?.platformAssets;
  return {
    windows: named?.windows ?? "",
    linux: named?.linux ?? "",
    macos: named?.macos ?? "",
  };
}

/** The map to advertise, or `undefined` when the host named no asset at all. */
function platformAssetsOf(draft: AssetDraft): PlatformAssets | undefined {
  const named: PlatformAssets = {
    windows: draft.windows.trim() || undefined,
    linux: draft.linux.trim() || undefined,
    macos: draft.macos.trim() || undefined,
  };
  const anyNamed = ASSET_FIELDS.some((field) => named[field.key] !== undefined);
  return anyNamed ? named : undefined;
}

/**
 * The host operator's suggestion of a TES3MP runtime for the players who
 * connect to this instance.
 *
 * It is advertised in the manifest summary and confirmed by a connecting
 * player before anything is downloaded — nothing is pushed, and the player can
 * pick something else. Only a GitHub release can be suggested: a path on this
 * machine means nothing on theirs, which is why this control is a repository
 * and a release rather than the full runtime picker.
 *
 * Under "advanced" is the per-platform asset name: which *file* of the release
 * is the runtime. Nerevar works that out from the official TES3MP naming for
 * itself, so these stay empty for a fork that follows it and are filled in by
 * one that does not.
 */
export function RuntimeHintSection({ instance }: { instance: InstanceConfig }) {
  const [draft, setDraft] = useState<GithubSource>(() => sourceFrom(instance));
  const [assets, setAssets] = useState<AssetDraft>(() => assetsFrom(instance));
  const [advanced, setAdvanced] = useState(
    () => platformAssetsOf(assetsFrom(instance)) !== undefined,
  );
  const [saving, setSaving] = useState(false);

  const hasHint = instance.runtimeHint != null;
  const repoIsUsable = normalizeRepo(draft.repo) !== null;
  const assetErrors = ASSET_FIELDS.map((field) =>
    assetNameError(assets[field.key]),
  );
  const assetsAreUsable = assetErrors.every((error) => error === null);

  const write = async (hint: RuntimeHint | null, message: string) => {
    setSaving(true);
    try {
      await invoke<RuntimeHint | null>("set_instance_runtime_hint", {
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

  const save = () => {
    const platformAssets = platformAssetsOf(assets);
    void write(
      {
        ...draft,
        repo: normalizeRepo(draft.repo) ?? draft.repo,
        ...(platformAssets ? { platformAssets } : {}),
      },
      "Runtime suggestion saved",
    );
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="font-serif text-sm text-foreground/60">
        Suggest this runtime to players. Their join screen offers to download it
        and says it came from you; they stay free to choose another.
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

      {advanced ? (
        <div className="flex flex-col gap-3 rounded-lg border border-border/50 bg-background/30 p-3">
          <p className="font-serif text-sm text-foreground/60">
            Which file of the release is the runtime, per platform. Leave a
            platform blank and Nerevar picks it by the official TES3MP asset
            naming; fill it in when this repository names its builds its own
            way.
          </p>
          {ASSET_FIELDS.map((field, index) => (
            <div key={field.key} className="space-y-1">
              <Label
                htmlFor={`runtime-hint-asset-${field.key}`}
                className="font-display text-[0.7rem] tracking-[0.25em] uppercase text-foreground/70"
              >
                {field.label}
              </Label>
              <Input
                id={`runtime-hint-asset-${field.key}`}
                value={assets[field.key]}
                spellCheck={false}
                autoCapitalize="none"
                autoCorrect="off"
                placeholder={field.placeholder}
                disabled={saving}
                className="font-mono text-xs"
                onChange={(event) =>
                  setAssets({ ...assets, [field.key]: event.target.value })
                }
              />
              {assetErrors[index] ? (
                <p className="font-mono text-xs text-destructive">
                  {assetErrors[index]}
                </p>
              ) : null}
            </div>
          ))}
        </div>
      ) : (
        <button
          type="button"
          className="flex w-fit items-center gap-2 font-display text-[0.7rem] tracking-[0.25em] text-foreground/60 uppercase hover:text-accent"
          onClick={() => setAdvanced(true)}
        >
          <Settings2 className="size-3.5" />
          Advanced: name the asset per platform
        </button>
      )}

      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          className="h-10 flex-1 font-display tracking-[0.15em] uppercase"
          disabled={saving || !repoIsUsable || !assetsAreUsable}
          onClick={save}
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
            setAssets({ ...EMPTY_ASSETS });
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
