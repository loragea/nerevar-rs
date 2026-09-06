import { z } from "zod";
import { hostAddressSchema } from "@/features/instances/schemas/host-address-schema";

/**
 * `isSynced` selects what the host field means. On a synced instance it is the
 * address of the Nerevar sync host, validated against the same rules
 * `nerevar-core` resolves it with. On an owned instance it is the TES3MP
 * server's advertised name, which is free text — spaces included — so it keeps
 * the lenient field it always had.
 */
export const instanceEditSchema = (isSynced: boolean) =>
  z.object({
    name: z
      .string()
      .trim()
      .min(1, "Name is required")
      .max(64, "Name must be at most 64 characters"),
    description: z
      .string()
      .max(500, "Description must be at most 500 characters"),
    host: isSynced
      ? hostAddressSchema
      : z
          .string()
          .trim()
          .min(1, "Host is required")
          .max(253, "Host is too long"),
    port: z
      .number()
      .int("Port must be a whole number")
      .min(1, "Port must be at least 1")
      .max(65535, "Port must be at most 65535"),
    password: z.string(),
  });

export type InstanceEditFormValues = z.infer<
  ReturnType<typeof instanceEditSchema>
>;
