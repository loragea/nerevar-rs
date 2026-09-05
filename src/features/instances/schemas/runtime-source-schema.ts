import { z } from "zod";

/**
 * The backend's `RuntimeSource` (see `src/types/RuntimeSource.ts`). Only the
 * `githubRelease` variant exists today; the object shape is what the form
 * carries so a future variant is a new branch here, not a new field.
 */
export const runtimeSourceSchema = z
  .object({
    kind: z.literal("githubRelease"),
    repo: z.string(),
    releaseId: z.string(),
    tag: z.string(),
    assetName: z.string(),
  })
  // Validated on the object rather than on `releaseId` so the message lands
  // on the field the picker is bound to.
  .refine((source) => source.releaseId.length > 0, {
    message: "Select a TES3MP release",
  });

export const TES3MP_REPO = "tes3mp/tes3mp";

export const emptyRuntimeSource = {
  kind: "githubRelease",
  repo: TES3MP_REPO,
  releaseId: "",
  tag: "",
  assetName: "",
} as const;
