import type {
  GithubAssetResponse,
  GithubReleaseResponse,
  RemoteManifestSummary,
  RuntimeHintResolution,
  RuntimeSource,
} from "@/types";
import { invoke } from "@tauri-apps/api/core";

/** The release file this machine would download, once one is known. */
export type HostRuntimeAsset = { name: string; sizeBytes: number };

export type HostRuntimeHint = {
  /** The value to put in the runtime picker. */
  runtime: RuntimeSource;
  /** One sentence for the screen: what the host suggested, and what came of it. */
  notice: string;
  /** False when the hinted repository is not one this player trusts. */
  trusted: boolean;
  /**
   * The asset the suggested release holds for this platform, when the release
   * was found and an asset could be chosen for it. `null` whenever there is
   * nothing definite to offer — an untrusted repository, a release that could
   * not be listed or matched, or a release with no asset this platform can
   * use — and the picker is then what the player gets.
   */
  asset: HostRuntimeAsset | null;
};

/**
 * Resolves the runtime a host advertises into a value for the runtime picker,
 * and into the one download a player can be asked to confirm.
 *
 * The repository is the *client's* choice and the tag is the server's: the
 * backend decides whether the hinted repository is one this player trusts
 * (`resolve_host_runtime_hint`, over the trusted-source list in Settings), and
 * only then is it preselected along with the release it names. A repository
 * the player has not trusted preselects nothing but the official one, with no
 * release chosen, and says so — Nerevar will not download from a repository a
 * server named.
 *
 * A trusted resolution also carries the asset file name the host chose for
 * this platform, if it named one; `select_runtime_asset` turns that (or, with
 * no name, the platform naming rules) into the actual release file, so the
 * screen can name the download and its size before anything is fetched.
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
      asset: null,
    };
  }

  const { repo, tag, assetName } = resolution;
  let releaseId = "";
  let resolvedTag = "";
  let asset: HostRuntimeAsset | null = null;
  let assetProblem = "";
  try {
    const releases = await invoke<GithubReleaseResponse[]>("get_all_releases", {
      repo,
    });
    const match =
      releases.find((release) => release.tag_name === tag) ??
      releases.find(
        (release) => release.id.toString() === resolution.releaseId,
      );
    if (match) {
      releaseId = match.id.toString();
      resolvedTag = match.tag_name;
      try {
        const chosen = await invoke<GithubAssetResponse>(
          "select_runtime_asset",
          { release: match, assetName: assetName || null },
        );
        asset = { name: chosen.name, sizeBytes: chosen.size };
      } catch (error) {
        // The release exists but holds nothing this platform can install, or
        // holds nothing under the name the host gave. The picker takes over,
        // and the reason travels with the notice.
        assetProblem = String(error);
      }
    }
  } catch {
    // The repository could not be listed (offline, renamed, private). The
    // repo still goes into the picker, which reports the failure itself.
  }

  let notice: string;
  if (releaseId.length === 0) {
    notice = `Suggested by the host: ${repo} ${tag || "(no release named)"} — not found in that repository; pick a release yourself.`;
  } else if (assetProblem.length > 0) {
    notice = `Suggested by the host: ${repo} ${resolvedTag}. ${assetProblem}`;
  } else {
    notice = `Suggested by the host: ${repo} ${resolvedTag}.`;
  }

  return {
    runtime: {
      kind: "githubRelease",
      repo,
      releaseId,
      tag: resolvedTag,
      assetName,
    },
    notice,
    trusted: true,
    asset,
  };
}
