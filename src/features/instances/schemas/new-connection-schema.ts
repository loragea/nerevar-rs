import { z } from "zod";
import { hostAddressSchema } from "@/features/instances/schemas/host-address-schema";
import {
  emptyRuntimeSource,
  runtimeSourceSchema,
} from "@/features/instances/schemas/runtime-source-schema";

export const newConnectionSchema = z.object({
  runtime: runtimeSourceSchema,
  connectionName: z
    .string()
    .trim()
    .min(1, "Connection name is required")
    .max(64, "Name must be at most 64 characters"),
  connectionDescription: z
    .string()
    .max(500, "Description must be at most 500 characters"),
  remoteHost: hostAddressSchema,
  remoteSyncPort: z
    .number()
    .int("Port must be a whole number")
    .min(1, "Port must be at least 1")
    .max(65535, "Port must be at most 65535"),
  syncPassword: z.string(),
});

export type NewConnectionFormValues = z.infer<typeof newConnectionSchema>;

export const newConnectionDefaultValues: NewConnectionFormValues = {
  runtime: { ...emptyRuntimeSource },
  connectionName: "",
  connectionDescription: "",
  remoteHost: "127.0.0.1",
  remoteSyncPort: 25567,
  syncPassword: "",
};
