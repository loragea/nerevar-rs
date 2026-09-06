import { Button } from "@/components/ui/button";
import { useBackgroundOperation } from "@/features/instances/context/background-operation-context";
import type { RuntimeMismatch } from "@/types";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, Download, Loader2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";

/**
 * The host requires a TES3MP version this instance does not have.
 *
 * The version is the host's to name; where the build comes from is not — the
 * update fetches the required tag from the repository this instance was
 * installed from, and `mismatch.message` says so when the host suggested a
 * repository the player has not trusted. A mismatch Nerevar cannot act on (a
 * runtime installed from the player's own folder or archive) has no update
 * button: there is no release list to look a tag up in.
 */
export function RuntimeMismatchNotice({
  instanceId,
  instanceName,
  mismatch,
  disabled,
  onUpdated,
}: {
  instanceId: string;
  instanceName: string;
  mismatch: RuntimeMismatch;
  disabled?: boolean;
  onUpdated: () => void;
}) {
  const { runOperation } = useBackgroundOperation();
  const [updating, setUpdating] = useState(false);

  const updateRuntime = async () => {
    setUpdating(true);
    try {
      await runOperation({
        instanceId,
        instanceName,
        kind: "updateRuntime",
        detail: `Installing TES3MP ${mismatch.requiredTag}`,
        task: (operationId) =>
          invoke<void>("update_instance_runtime", {
            instanceId,
            requiredTag: mismatch.requiredTag,
            operationId,
          }),
      });
      toast.success(`TES3MP ${mismatch.requiredTag} installed`);
      onUpdated();
    } catch (error) {
      toast.error(`Runtime update failed: ${error}`);
    } finally {
      setUpdating(false);
    }
  };

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-destructive/40 bg-destructive/10 p-4">
      <p className="flex items-start gap-2 font-serif text-sm leading-relaxed text-destructive">
        <AlertTriangle className="mt-0.5 size-4 shrink-0" />
        <span>{mismatch.message}</span>
      </p>
      {mismatch.enforced ? (
        <Button
          variant="outline"
          className="h-10 w-full font-display text-base tracking-[0.15em] uppercase"
          disabled={disabled || updating}
          onClick={() => void updateRuntime()}
        >
          {updating ? (
            <Loader2 className="animate-spin" data-icon="inline-start" />
          ) : (
            <Download data-icon="inline-start" />
          )}
          Update runtime
        </Button>
      ) : null}
    </div>
  );
}
