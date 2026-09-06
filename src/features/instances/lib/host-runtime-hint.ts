import { TES3MP_REPO } from "@/features/instances/schemas/runtime-source-schema";
import type {
  GithubReleaseResponse,
  RemoteManifestSummary,
  RuntimeSource,
} from "@/types";
import { invoke } from "@tauri-apps/api/core";

/** What the host advertised, kept only so a screen can say so. */
export type HostRuntimeSuggestion = {
  repo: string;
  tag: string;
  /** True when the named release still exists in that repository. */
  resolved: boolean;
};

export type HostRuntimeHint = {
  /** The value to put in the runtime picker. */
  runtime: RuntimeSource;
  suggestion: HostRuntimeSuggestion;
};

/**
 * Resolves the runtime a host advertises into a value for the runtime picker.
 *
 * A suggestion is a starting point, never an instruction: the picker is set to
 * the named GitHub repository (and to the named release when that repo still
 * publishes it), the caller says where the choice came from, and every part of
 * it stays editable. A host that advertises nothing returns `null` and leaves
 * the field exactly as it was.
 *
 * Shared by the New Connection page and onboarding's joining path so both
 * preselect the same way.
 */
export async function resolveHostRuntimeHint(
  summary: RemoteManifestSummary,
): Promise<HostRuntimeHint | null> {
  const hint = summary.runtimeHint;
  if (!hint || hint.kind !== "githubRelease") {
    return null;
  }

  const repo = hint.repo || TES3MP_REPO;
  let releaseId = "";
  let tag = "";
  try {
    const releases = await invoke<GithubReleaseResponse[]>("get_all_releases", {
      repo,
    });
    const match =
      releases.find((release) => release.tag_name === hint.tag) ??
      releases.find((release) => release.id.toString() === hint.releaseId);
    if (match) {
      releaseId = match.id.toString();
      tag = match.tag_name;
    }
  } catch {
    // The repository could not be listed (offline, renamed, private). The
    // repo still goes into the picker, which reports the failure itself.
  }

  return {
    runtime: { kind: "githubRelease", repo, releaseId, tag, assetName: "" },
    suggestion: {
      repo,
      tag: hint.tag,
      resolved: releaseId.length > 0,
    },
  };
}
