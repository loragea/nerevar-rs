import { z } from "zod";

// One side of an `owner/name` pair: GitHub's alphabet for account and
// repository names.
const REPO_SEGMENT = /^[A-Za-z0-9._-]+$/;

/**
 * Normalises a repository the user typed into the `owner/name` form the
 * releases API takes, or returns `null` when it is not one. Mirrors
 * `runtime::source::normalize_repo` in the backend, which validates again —
 * this copy exists so the picker can say "not a repository" without a round
 * trip, not so the backend can trust it.
 *
 * A pasted browser URL is the one shape rewritten rather than rejected.
 */
export function normalizeRepo(input: string): string | null {
  const trimmed = input.trim();
  if (trimmed.length === 0) return null;

  const withoutHost = trimmed
    .replace(/^https?:\/\//, "")
    .replace(/^(www\.)?github\.com\//, "");
  const candidate = withoutHost.replace(/\/+$/, "").replace(/\.git$/, "");

  const parts = candidate.split("/");
  if (parts.length !== 2) return null;
  const [owner, name] = parts;
  if (!REPO_SEGMENT.test(owner) || !REPO_SEGMENT.test(name)) return null;
  if (owner === "." || owner === ".." || name === "." || name === "..") {
    return null;
  }
  return `${owner}/${name}`;
}

/**
 * The backend's `RuntimeSource` (see `src/types/RuntimeSource.ts`): a
 * discriminated union on `kind`, one branch per way a TES3MP runtime can
 * reach an instance. Whichever branch is chosen, the runtime is installed
 * *into* the instance's `tes3mp/` directory — nothing is run in place.
 */
export const runtimeSourceSchema = z.discriminatedUnion("kind", [
  z
    .object({
      kind: z.literal("githubRelease"),
      repo: z.string(),
      releaseId: z.string(),
      tag: z.string(),
      assetName: z.string(),
    })
    // Validated on the object rather than on `releaseId` so the message lands
    // on the field the picker is bound to.
    .refine((source) => normalizeRepo(source.repo) !== null, {
      message: "Enter the GitHub repository as owner/name",
    })
    .refine((source) => source.releaseId.length > 0, {
      message: "Select a TES3MP release",
    }),
  z.object({
    kind: z.literal("localDirectory"),
    path: z.string().min(1, "Choose the folder holding your TES3MP install"),
  }),
  z.object({
    kind: z.literal("archive"),
    path: z.string().min(1, "Choose a TES3MP release archive"),
  }),
]);

export type RuntimeSourceKind = z.infer<typeof runtimeSourceSchema>["kind"];

export const TES3MP_REPO = "tes3mp/tes3mp";

export const emptyRuntimeSource = {
  kind: "githubRelease",
  repo: TES3MP_REPO,
  releaseId: "",
  tag: "",
  assetName: "",
} as const;

/**
 * The form value a kind starts from when the user switches to it. Switching
 * away and back is a reset, not a restore: a half-filled branch the user
 * abandoned is not what they meant to submit.
 */
export function emptySourceOfKind(kind: RuntimeSourceKind) {
  switch (kind) {
    case "githubRelease":
      return { ...emptyRuntimeSource };
    case "localDirectory":
      return { kind: "localDirectory" as const, path: "" };
    case "archive":
      return { kind: "archive" as const, path: "" };
  }
}
