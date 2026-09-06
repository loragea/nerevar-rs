import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Field, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { SyncProgressPanel } from "@/features/instances/components/sync-progress-panel";
import { useInstanceSync } from "@/features/instances/hooks/use-instance-sync";
import { HOST_ADDRESS_HELP } from "@/features/instances/schemas/host-address-schema";
import {
  newConnectionDefaultValues,
  newConnectionSchema,
  type NewConnectionFormValues,
} from "@/features/instances/schemas/new-connection-schema";
import { useConfig } from "@/features/config/context/config-context-provider";
import { RuntimeSourceField } from "@/features/instances/components/runtime-source-field";
import { useBackgroundOperation } from "@/features/instances/context/background-operation-context";
import { formatByteSize } from "@/lib/format";
import { TES3MP_REPO } from "@/features/instances/schemas/runtime-source-schema";
import type {
  GithubReleaseResponse,
  NewConnectionConfig,
  RemoteManifestSummary,
} from "@/types";
import { standardSchemaResolver } from "@hookform/resolvers/standard-schema";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ArrowLeft, Loader2, Radio, Sparkles } from "lucide-react";
import { useEffect, useState } from "react";
import { Controller, useForm } from "react-hook-form";
import { toast } from "sonner";
import { Link } from "wouter";
import { navigate } from "wouter/use-browser-location";

const SECTION_LABEL =
  "text-lg font-light font-display tracking-[0.08em] text-foreground";

export function NewConnectionPage() {
  const config = useConfig();
  const { runOperation } = useBackgroundOperation();
  const [testing, setTesting] = useState(false);
  const [creating, setCreating] = useState(false);
  // A local runtime the backend would reject: the submit stays disabled
  // rather than letting the create fail after the copy.
  const [runtimeBlocked, setRuntimeBlocked] = useState(false);
  const [preview, setPreview] = useState<RemoteManifestSummary | null>(null);
  // What the host suggested, kept only to say so on screen: the suggestion
  // is never stored anywhere but the `runtime` the user ends up confirming.
  const [hostSuggestion, setHostSuggestion] = useState<{
    repo: string;
    tag: string;
    resolved: boolean;
  } | null>(null);
  const [syncInstanceId, setSyncInstanceId] = useState("");
  // Where the instance would land, derived by the backend — the only place
  // that knows whether this platform separates paths with "/" or "\".
  const [instancePath, setInstancePath] = useState("");

  const sync = useInstanceSync(syncInstanceId);

  const form = useForm<NewConnectionFormValues>({
    resolver: standardSchemaResolver(newConnectionSchema),
    defaultValues: {
      ...newConnectionDefaultValues,
      remoteSyncPort: config?.syncPort ?? 25567,
    },
    mode: "onSubmit",
  });

  const connectionName = form.watch("connectionName");
  const nerevarRoot = config?.rootPath ?? "";

  useEffect(() => {
    let cancelled = false;
    void invoke<string>("preview_connection_instance_path", {
      rootPath: nerevarRoot,
      connectionName,
    })
      .then((path) => {
        if (!cancelled) setInstancePath(path);
      })
      .catch(() => {
        // No name typed yet, or nothing usable survives sanitising: there is
        // no instance folder to name, so the data directory stands alone.
        if (!cancelled) setInstancePath(nerevarRoot);
      });
    return () => {
      cancelled = true;
    };
  }, [connectionName, nerevarRoot]);

  /**
   * Preselects the runtime the host advertises, if it advertises one.
   *
   * A suggestion is a starting point, never an instruction: the picker is set
   * to the named GitHub repository (and to the named release when that repo
   * still publishes it), the page says where the choice came from, and every
   * part of it stays editable. A host that advertises nothing leaves the
   * field exactly as it was.
   */
  const applyHostRuntimeHint = async (summary: RemoteManifestSummary) => {
    const hint = summary.runtimeHint;
    if (!hint || hint.kind !== "githubRelease") {
      setHostSuggestion(null);
      return;
    }

    const repo = hint.repo || TES3MP_REPO;
    let releaseId = "";
    let tag = "";
    try {
      const releases = await invoke<GithubReleaseResponse[]>(
        "get_all_releases",
        { repo },
      );
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

    form.setValue(
      "runtime",
      { kind: "githubRelease", repo, releaseId, tag, assetName: "" },
      { shouldValidate: false },
    );
    setHostSuggestion({
      repo,
      tag: hint.tag,
      resolved: releaseId.length > 0,
    });
  };

  const testConnection = async () => {
    const values = form.getValues();
    const parsed = newConnectionSchema.safeParse(values);
    if (!parsed.success) {
      form.trigger();
      return;
    }

    setTesting(true);
    setPreview(null);
    try {
      await invoke("ping_remote_nerevar_server", {
        remoteHost: parsed.data.remoteHost,
        remoteSyncPort: parsed.data.remoteSyncPort,
      });
      const summary = await invoke<RemoteManifestSummary>(
        "fetch_remote_manifest_summary",
        {
          remoteHost: parsed.data.remoteHost,
          remoteSyncPort: parsed.data.remoteSyncPort,
          syncPassword: parsed.data.syncPassword || null,
        },
      );
      setPreview(summary);
      await applyHostRuntimeHint(summary);
      toast.success(`Connected to ${summary.instanceName}`);
    } catch (error) {
      toast.error(`Connection test failed: ${error}`);
    } finally {
      setTesting(false);
    }
  };

  const onSubmit = form.handleSubmit(async (values) => {
    setCreating(true);
    try {
      const payload: NewConnectionConfig = {
        runtime: values.runtime,
        connectionName: values.connectionName,
        connectionDescription: values.connectionDescription,
        // Just the Nerevar data directory: the command derives the
        // instance's own paths from it and the connection name.
        rootPath: nerevarRoot,
        remoteHost: values.remoteHost,
        remoteSyncPort: values.remoteSyncPort,
        syncPassword: values.syncPassword,
      };

      // Through `runOperation` so the runtime install's progress events —
      // download, copy, extract — land on the banner: the create mints the
      // operation id here and hands it to the command, which forwards it to
      // `runtime::acquire`.
      const instanceId = await runOperation({
        instanceId: "",
        instanceName: values.connectionName,
        kind: "createConnection",
        detail: "Installing the TES3MP runtime",
        task: (operationId) =>
          invoke<string>("add_synced_connection", {
            newConnection: payload,
            operationId,
          }),
      });
      setSyncInstanceId(instanceId);
      toast.success("Connection created — starting initial sync");
      await sync.startSync(instanceId);
      navigate(`/instances/${encodeURIComponent(instanceId)}`);
    } catch (error) {
      if (!String(error).toLowerCase().includes("cancelled")) {
        toast.error(`Failed to create connection: ${error}`);
      }
    } finally {
      setCreating(false);
      setSyncInstanceId("");
    }
  });

  useEffect(() => {
    const unlisten = listen("on_config_added_connection", () => {
      // Config refresh handled by provider
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  const busy = testing || creating || sync.syncing;

  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-5 px-1 pb-10">
      <Button
        variant="outline"
        size="sm"
        className="w-fit font-display text-xs tracking-[0.2em] text-foreground/70 uppercase hover:text-accent"
        asChild
      >
        <Link href="/synced-instances">
          <ArrowLeft data-icon="inline-start" />
          Synced instances
        </Link>
      </Button>

      <Card className="gap-0 border-border/80 bg-card/70 py-0 ring-1 ring-accent/20">
        <CardContent className="flex flex-col gap-6 py-6">
          <div>
            <h1 className="font-display text-2xl tracking-[0.08em] text-gradient-gold">
              New connection
            </h1>
            <p className="mt-2 font-serif text-sm text-foreground/75">
              Connect to a Nerevar host, download its manifest, and sync mod
              data into a local TES3MP instance.
            </p>
          </div>

          <form onSubmit={onSubmit} className="flex flex-col gap-5">
            <Field>
              <FieldLabel className={SECTION_LABEL}>TES3MP runtime</FieldLabel>
              <Controller
                control={form.control}
                name="runtime"
                render={({ field, fieldState }) => (
                  <>
                    <RuntimeSourceField
                      value={field.value}
                      onValueChange={field.onChange}
                      disabled={busy}
                      onBlockingChange={setRuntimeBlocked}
                    />
                    {hostSuggestion ? (
                      <p className="text-left font-mono text-xs text-accent/90">
                        Suggested by the host: {hostSuggestion.repo}{" "}
                        {hostSuggestion.tag || "(no release named)"}
                        {hostSuggestion.resolved
                          ? ""
                          : " — not found in that repository; pick a release yourself"}
                      </p>
                    ) : null}
                    {fieldState.error ? (
                      <FieldError errors={[fieldState.error]} />
                    ) : null}
                  </>
                )}
              />
            </Field>

            <Field>
              <FieldLabel className={SECTION_LABEL} htmlFor="connectionName">
                Connection name
              </FieldLabel>
              <Input
                id="connectionName"
                disabled={busy}
                {...form.register("connectionName")}
              />
              {form.formState.errors.connectionName ? (
                <FieldError errors={[form.formState.errors.connectionName]} />
              ) : null}
            </Field>

            <Field>
              <FieldLabel
                className={SECTION_LABEL}
                htmlFor="connectionDescription"
              >
                Description
              </FieldLabel>
              <Textarea
                id="connectionDescription"
                disabled={busy}
                {...form.register("connectionDescription")}
              />
            </Field>

            <div className="grid gap-4 sm:grid-cols-2">
              <Field>
                <FieldLabel className={SECTION_LABEL} htmlFor="remoteHost">
                  Nerevar host
                </FieldLabel>
                <Input
                  id="remoteHost"
                  disabled={busy}
                  {...form.register("remoteHost")}
                />
                <p className="font-serif text-sm text-foreground/60">
                  {HOST_ADDRESS_HELP}
                </p>
                {form.formState.errors.remoteHost ? (
                  <FieldError errors={[form.formState.errors.remoteHost]} />
                ) : null}
              </Field>
              <Field>
                <FieldLabel className={SECTION_LABEL} htmlFor="remoteSyncPort">
                  Sync port
                </FieldLabel>
                <Input
                  id="remoteSyncPort"
                  type="number"
                  disabled={busy}
                  {...form.register("remoteSyncPort", { valueAsNumber: true })}
                />
                {form.formState.errors.remoteSyncPort ? (
                  <FieldError errors={[form.formState.errors.remoteSyncPort]} />
                ) : null}
              </Field>
            </div>

            <Field>
              <FieldLabel className={SECTION_LABEL} htmlFor="syncPassword">
                Sync password
              </FieldLabel>
              <Input
                id="syncPassword"
                type="password"
                autoComplete="off"
                disabled={busy}
                {...form.register("syncPassword")}
              />
              <p className="font-serif text-sm text-foreground/60">
                Required when the host TES3MP server has a password. Also written
                to tes3mp-client-default.cfg for game connection.
              </p>
            </Field>

            <Field>
              <FieldLabel className={SECTION_LABEL} htmlFor="instanceRootPath">
                Local instance path
              </FieldLabel>
              <Input
                id="instanceRootPath"
                value={instancePath}
                readOnly
                disabled
              />
            </Field>

            {preview ? (
              <div className="rounded-lg border border-accent/30 bg-accent/5 p-4 space-y-2">
                <p className="font-display text-xs tracking-[0.15em] text-accent uppercase">
                  Remote manifest preview
                </p>
                <p className="font-serif text-sm text-foreground/80">
                  {preview.instanceName} — {preview.packageCount} package(s),{" "}
                  {formatByteSize(preview.totalDownloadBytes)} total
                </p>
                <p className="font-mono text-xs text-accent/80">
                  TES3MP server port: {preview.tes3mpServerPort}
                  {preview.passwordRequired ? " · password required" : ""}
                </p>
                <ul className="font-mono text-[0.65rem] text-foreground/60 space-y-1 max-h-24 overflow-y-auto">
                  {preview.packages.map((pkg) => (
                    <li key={pkg.id}>
                      {pkg.name} ({formatByteSize(pkg.totalSizeBytes)})
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}

            {syncInstanceId &&
            (sync.syncing ||
              sync.progress ||
              sync.resumeStatus?.canResume) ? (
              <SyncProgressPanel
                progress={sync.progress}
                syncing={sync.syncing}
                resumeStatus={sync.resumeStatus}
                onCancel={() => void sync.cancelSync()}
              />
            ) : null}

            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                disabled={busy}
                onClick={() => void testConnection()}
              >
                {testing ? (
                  <Loader2 className="animate-spin" data-icon="inline-start" />
                ) : (
                  <Radio data-icon="inline-start" />
                )}
                Test connection
              </Button>
              <Button
                type="submit"
                variant="launch"
                disabled={busy || runtimeBlocked}
                className="flex-1"
              >
                {creating || sync.syncing ? (
                  <Loader2 className="animate-spin" data-icon="inline-start" />
                ) : (
                  <Sparkles data-icon="inline-start" />
                )}
                Create & sync
              </Button>
            </div>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
