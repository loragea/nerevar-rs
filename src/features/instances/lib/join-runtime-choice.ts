import type { HostRuntimeHint } from "@/features/instances/lib/host-runtime-hint";

/**
 * Which runtime screen the join flow shows once a host has been reached.
 *
 * `confirm` is the one-press case: the host suggested a build from a
 * repository this player trusts, the release exists, and the file to download
 * is known — so there is one definite thing to agree to and nothing to
 * operate. Anything less definite is `pick`, which is the runtime picker the
 * flow has always shown.
 */
export type JoinRuntimeScreen =
  | {
      kind: "confirm";
      /** `owner/name` of the repository the build comes from. */
      repo: string;
      /** The TES3MP version, as the release tags it. */
      tag: string;
      /** The release file that would be downloaded. */
      assetName: string;
      /** Its size, for the sentence the player confirms. */
      sizeBytes: number;
    }
  | { kind: "pick" };

/**
 * Decides that screen from a resolved host hint.
 *
 * Deliberately free of React and of any Tauri call: everything it needs is
 * already in the hint, and keeping it separate means the rule that says "one
 * confirmation, not a picker" can be read (and later tested) on its own.
 */
export function joinRuntimeScreen(
  hint: HostRuntimeHint | null,
): JoinRuntimeScreen {
  if (hint === null || !hint.trusted || hint.asset === null) {
    return { kind: "pick" };
  }
  const { runtime } = hint;
  if (runtime.kind !== "githubRelease") {
    return { kind: "pick" };
  }
  // No tag names no version to confirm, and no release id means the picker
  // has nothing selected — either way there is no single build to agree to.
  if (runtime.tag.length === 0 || runtime.releaseId.length === 0) {
    return { kind: "pick" };
  }
  return {
    kind: "confirm",
    repo: runtime.repo,
    tag: runtime.tag,
    assetName: hint.asset.name,
    sizeBytes: hint.asset.sizeBytes,
  };
}
