import type {
  InstanceSyncStatus,
  RuntimeMismatch,
  SyncOutcome,
  SyncProgressEvent,
} from "@/types";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";

const TERMINAL_SYNC_PHASES = new Set<SyncProgressEvent["phase"]>([
  "complete",
  "cancelled",
  "failed",
]);

export function useInstanceSync(instanceId: string) {
  const [progress, setProgress] = useState<SyncProgressEvent | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [resumeStatus, setResumeStatus] = useState<InstanceSyncStatus | null>(
    null,
  );
  const [lastOutcome, setLastOutcome] = useState<SyncOutcome | null>(null);
  // The TES3MP version this instance's host requires, when it is not the one
  // installed. Set from core's `runtime-mismatch` event so it is picked up
  // whether the sync was started from the Sync button or by a launch.
  const [runtimeMismatch, setRuntimeMismatch] =
    useState<RuntimeMismatch | null>(null);

  const refreshResumeStatus = useCallback(async () => {
    if (!instanceId) {
      setResumeStatus(null);
      return;
    }
    try {
      const status = await invoke<InstanceSyncStatus>(
        "get_instance_sync_status",
        { instanceId },
      );
      setResumeStatus(status);
    } catch {
      setResumeStatus(null);
    }
  }, [instanceId]);

  useEffect(() => {
    void refreshResumeStatus();
  }, [refreshResumeStatus]);

  useEffect(() => {
    const unlisten = listen<RuntimeMismatch>("runtime-mismatch", (event) => {
      if (event.payload.instanceId !== instanceId) return;
      setRuntimeMismatch(event.payload);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [instanceId]);

  useEffect(() => {
    const unlisten = listen<SyncProgressEvent>("sync-progress", (event) => {
      if (event.payload.instanceId !== instanceId) return;
      setProgress(event.payload);
      setSyncing(!TERMINAL_SYNC_PHASES.has(event.payload.phase));
      if (TERMINAL_SYNC_PHASES.has(event.payload.phase)) {
        void refreshResumeStatus();
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [instanceId, refreshResumeStatus]);

  const startSync = useCallback(async (overrideInstanceId?: string) => {
    const targetId = overrideInstanceId ?? instanceId;
    if (!targetId) {
      throw new Error("No instance id");
    }
    setSyncing(true);
    setLastOutcome(null);
    setRuntimeMismatch(null);
    setProgress(null);
    try {
      const outcome = await invoke<SyncOutcome>("sync_instance_from_remote", {
        instanceId: targetId,
      });
      setLastOutcome(outcome);
      setRuntimeMismatch(outcome.runtimeMismatch ?? null);
      if (!outcome.validation.valid) {
        toast.error(
          `Sync finished with ${outcome.validation.issues.length} validation issue(s)`,
        );
      } else if (outcome.runtimeMismatch) {
        toast.warning(outcome.runtimeMismatch.message);
      } else {
        toast.success("Sync complete — files verified");
      }
      return outcome;
    } catch (error) {
      const message = String(error);
      if (!message.toLowerCase().includes("cancelled")) {
        toast.error(`Sync failed: ${error}`);
      } else {
        toast.info("Sync paused — run Sync again to resume");
      }
      setSyncing(false);
      void refreshResumeStatus();
      throw error;
    }
  }, [instanceId, refreshResumeStatus]);

  const cancelSync = useCallback(async () => {
    try {
      await invoke<boolean>("cancel_instance_sync", { instanceId });
      toast.info("Stopping sync — progress will be saved");
    } catch (error) {
      toast.error(`Failed to cancel sync: ${error}`);
    }
  }, [instanceId]);

  return {
    progress,
    syncing,
    resumeStatus,
    lastOutcome,
    runtimeMismatch,
    clearRuntimeMismatch: useCallback(() => setRuntimeMismatch(null), []),
    refreshResumeStatus,
    startSync,
    cancelSync,
  };
}
