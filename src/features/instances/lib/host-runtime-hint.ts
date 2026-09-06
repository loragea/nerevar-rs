import type {
  GithubReleaseResponse,
  RemoteManifestSummary,
  RuntimeHintResolution,
  RuntimeSource,
} from "@/types";
import { invoke } from "@tauri-apps/api/core";

export type HostRuntimeHint = {
  /** The value to put in the runtime picker. */
  runtime: RuntimeSource;
  /** One sentence for the screen: what the host suggested, and what came of it. */
  notice: string;
  /** False when the hinted repository is not one this player trusts. */
  trusted: boolean;
};

/**
 * Resolves the runtime a host advertises into a value for the runtime picker.
 *
 * The repository is the *client's* choice and the tag is the server's: the
 * backend decides whether the hinted repository is one this player trusts
 * (`resolve_host_runtime_hint`, over the trusted-source list in Settings), and
 * only then is it preselected along with the release it names. A repository
 * the player has not trusted preselects nothing but the official one, with no
 * release chosen, and says so — Nerevar will not download from a repository a
 * server named.
 *
 * A host that advertises nothing returns `null` and leaves the field exactly
 * as it was.
 *
 * Shared by the New Connection page and onboarding's joining path so both
 * preselect the same way.
 */
export async function resolveHostRuntimeHint(
  summary: RemoteManifestSummary,
): Promise<HostRuntimeHint | null> {
  const resolution = await invoke<RuntimeHintResolution>(
    "resolve_host_runtime_hint",
    { hint: summary.runtimeHint ?? null },
  );

  if (resolution.status === "noHint") {
    return null;
  }

  if (resolution.status === "untrusted") {
    return {
      runtime: {
        kind: "githubRelease",
        repo: resolution.fallbackRepo,
        releaseId: "",
        tag: "",
        assetName: "",
      },
      notice: resolution.message,
      trusted: false,
    };
  }

  const { repo, tag } = resolution;
  let releaseId = "";
  let resolvedTag = "";
  try {
    const releases = await invoke<GithubReleaseResponse[]>("get_all_releases", {
      repo,
    });
    const match =
      releases.find((release) => release.tag_name === tag) ??
      releases.find((release) => release.id.toString() === resolution.releaseId);
    if (match) {
      releaseId = match.id.toString();
      resolvedTag = match.tag_name;
    }
  } catch {
    // The repository could not be listed (offline, renamed, private). The
    // repo still goes into the picker, which reports the failure itself.
  }

  return {
    runtime: {
      kind: "githubRelease",
      repo,
      releaseId,
      tag: resolvedTag,
      assetName: "",
    },
    notice:
      releaseId.length > 0
        ? `Suggested by the host: ${repo} ${resolvedTag}.`
        : `Suggested by the host: ${repo} ${tag || "(no release named)"} — not found in that repository; pick a release yourself.`,
    trusted: true,
  };
}
