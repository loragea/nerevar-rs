import { NerevarHeader } from "@/components/custom/nerevar-header";
import { Button } from "@/components/ui/button";
import {
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useConfig } from "@/features/config/context/config-context-provider";
import { RuntimeSourceField } from "@/features/instances/components/runtime-source-field";
import { useBackgroundOperation } from "@/features/instances/context/background-operation-context";
import {
  resolveHostRuntimeHint,
  type HostRuntimeHint,
} from "@/features/instances/lib/host-runtime-hint";
import { hostAddressError } from "@/features/instances/schemas/host-address-schema";
import {
  emptyRuntimeSource,
  runtimeSourceSchema,
} from "@/features/instances/schemas/runtime-source-schema";
import {
  EASE,
  OnboardingProgress,
  OnboardingStepCard,
  slideVariants,
  StepActions,
} from "@/features/onboarding/components/onboarding-step-shell";
import { formatByteSize } from "@/lib/format";
import { exampleDataDirPath } from "@/lib/platform";
import type {
  MorrowindCandidate,
  MorrowindCandidateSource,
  NewConnectionConfig,
  RemoteManifestSummary,
  RuntimeSource,
} from "@/types";
import { invoke } from "@tauri-apps/api/core";
import {
  Download,
  FolderOpen,
  Gamepad2,
  Play,
  Plug,
  Settings2,
  ShieldCheck,
} from "lucide-react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { useEffect, useState } from "react";
import { navigate } from "wouter/use-browser-location";

/**
 * Onboarding for a player joining someone else's server.
 *
 * Four screens, in the order a player can answer them: where Morrowind is
 * (found for them where possible), where Nerevar may keep its files (a
 * default, with the picker behind "advanced"), what happens when they play,
 * and the address their friend sent. The last one creates the instance and
 * finishes onboarding, so there is no separate "done" screen — the app lands
 * on the server's own page instead.
 */
export type JoinStage = "morrowind" | "data-dir" | "guide" | "connect";

const JOIN_STAGES: { id: JoinStage; label: string }[] = [
  { id: "morrowind", label: "Morrowind" },
  { id: "data-dir", label: "Folder" },
  { id: "guide", label: "Guide" },
  { id: "connect", label: "Connect" },
];

export function JoinFlow({ onFinish }: { onFinish: () => void }) {
  const reduceMotion = useReducedMotion();
  const [stage, setStage] = useState<JoinStage>("morrowind");
  // Held here rather than read back from config: `set_root_path` writes the
  // file without emitting a config change, so the context would still be
  // showing the old value on the next screen.
  const [dataDir, setDataDir] = useState("");
  const stageIndex = JOIN_STAGES.findIndex((s) => s.id === stage);

  return (
    <div className="flex h-full w-full flex-col items-center justify-center px-4 py-10">
      <NerevarHeader title="NEREVAR" subtitle="Setup" />

      <OnboardingProgress steps={JOIN_STAGES} currentIndex={stageIndex} />

      <div className="relative mt-8 w-full max-w-lg">
        <AnimatePresence mode="wait" custom={1}>
          <motion.div
            key={stage}
            custom={1}
            variants={reduceMotion ? undefined : slideVariants}
            initial={reduceMotion ? false : "enter"}
            animate={reduceMotion ? undefined : "center"}
            exit={reduceMotion ? undefined : "exit"}
            transition={{ duration: 0.28, ease: EASE }}
            className="w-full"
          >
            {stage === "morrowind" && (
              <JoinMorrowindStep onNext={() => setStage("data-dir")} />
            )}
            {stage === "data-dir" && (
              <JoinDataDirStep
                onNext={(dir) => {
                  setDataDir(dir);
                  setStage("guide");
                }}
              />
            )}
            {stage === "guide" && (
              <JoinGuideStep
                onNext={() => setStage("connect")}
                onBack={() => setStage("data-dir")}
              />
            )}
            {stage === "connect" && (
              <JoinConnectStep
                dataDir={dataDir}
                onBack={() => setStage("guide")}
                onJoined={(instanceId) => {
                  navigate(`/instances/${encodeURIComponent(instanceId)}`);
                  onFinish();
                }}
              />
            )}
          </motion.div>
        </AnimatePresence>
      </div>
    </div>
  );
}

const CANDIDATE_SOURCE_LABELS: Record<MorrowindCandidateSource, string> = {
  steamLibrary: "in your Steam library",
  gogInstall: "in your GOG games",
  openmwConfig: "in your OpenMW configuration",
  windowsRegistry: "in the Windows registry",
};

/**
 * Step 1: the Morrowind installation.
 *
 * Nerevar looks in the usual places first and asks the player to confirm what
 * it found; the file picker — which used to be the only way through this
 * screen — is one button away, and is what a player with no detected install
 * sees straight away.
 */
function JoinMorrowindStep({ onNext }: { onNext: () => void }) {
  const [candidates, setCandidates] = useState<MorrowindCandidate[] | null>(
    null,
  );
  const [pickedPath, setPickedPath] = useState("");
  const [picking, setPicking] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void invoke<MorrowindCandidate[]>("find_morrowind_installations")
      .then((found) => {
        if (cancelled) return;
        setCandidates(found);
        setPicking(found.length === 0);
      })
      .catch(() => {
        if (cancelled) return;
        setCandidates([]);
        setPicking(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const suggestion = candidates?.[0] ?? null;
  const chosenPath = picking ? pickedPath : (suggestion?.path ?? "");

  const handleBrowse = async () => {
    setError(null);
    try {
      const path = await invoke<string>("open_esm_file_picker");
      if (!path.match(/[/\\]Morrowind\.esm$/i)) {
        setError("That file is not Morrowind.esm.");
        return;
      }
      setPickedPath(path.replace(/[/\\]Morrowind\.esm$/i, ""));
    } catch {
      setError("No file selected.");
    }
  };

  const handleConfirm = () => {
    setSaving(true);
    setError(null);
    invoke<void>("set_morrowind_data_files", {
      morrowindDataFiles: chosenPath,
    })
      .then(() => onNext())
      .catch((err) => setError(String(err)))
      .finally(() => setSaving(false));
  };

  return (
    <OnboardingStepCard>
      <CardHeader className="border-b border-border/50 px-6 pb-4 pt-6">
        <div className="mb-3 flex size-10 items-center justify-center rounded-lg border border-accent/40 bg-accent/10 text-accent">
          <Gamepad2 className="size-5" />
        </div>
        <CardTitle className="font-display text-base font-bold tracking-[0.2em] text-accent uppercase">
          Your Morrowind
        </CardTitle>
        <CardDescription className="font-serif text-base font-light tracking-[0.05em] leading-relaxed text-foreground/75">
          Nerevar needs the game you already own. Your own OpenMW setup stays
          untouched — Nerevar keeps its own copy of the configuration and puts
          yours back after you play.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4 px-6 py-5">
        {candidates === null ? (
          <p className="font-serif text-sm text-foreground/70">
            Looking for Morrowind...
          </p>
        ) : null}

        {candidates !== null && !picking && suggestion ? (
          <div className="flex flex-col gap-3">
            <p className="font-serif text-sm text-foreground/75">
              Found Morrowind {CANDIDATE_SOURCE_LABELS[suggestion.source]}. Is
              this the right one?
            </p>
            <p className="rounded-lg border border-border/50 bg-background/30 p-3 font-mono text-xs break-all text-accent">
              {suggestion.path}
            </p>
            <Button
              type="button"
              variant="outline"
              className="w-fit font-display text-[0.75rem] tracking-[0.3em] uppercase"
              onClick={() => {
                setPicking(true);
                setError(null);
              }}
            >
              Choose another
            </Button>
          </div>
        ) : null}

        {candidates !== null && picking ? (
          <div className="flex flex-col gap-3">
            <p className="font-serif text-sm text-foreground/75">
              {candidates.length === 0
                ? "Nerevar could not find Morrowind on its own. Point it at your Morrowind.esm file."
                : "Point Nerevar at the Morrowind.esm file you want to use."}
            </p>
            <div className="space-y-2">
              <Label
                htmlFor="join-morrowind-path"
                className="font-display text-[0.75rem] tracking-[0.3em] uppercase text-foreground/70"
              >
                Data Files
              </Label>
              <div className="flex gap-2">
                <Input
                  id="join-morrowind-path"
                  readOnly
                  value={pickedPath}
                  placeholder="No folder chosen yet"
                  className="font-mono text-xs bg-input/40 truncate"
                />
                <Button
                  type="button"
                  variant="outline"
                  className="shrink-0 font-display text-[0.75rem] tracking-[0.3em] uppercase"
                  onClick={() => void handleBrowse()}
                >
                  Browse
                </Button>
              </div>
            </div>
          </div>
        ) : null}

        {error ? <StepError message={error} /> : null}
      </CardContent>
      <StepActions
        onPrimary={handleConfirm}
        primaryLabel={
          saving ? "Setting up" : picking ? "Use this folder" : "Use this"
        }
        showBack={false}
        nextDisabled={saving || chosenPath.length === 0}
      />
    </OnboardingStepCard>
  );
}

/**
 * Step 2: where Nerevar keeps its files.
 *
 * A player has no reason to have an opinion about this, so there is a default
 * and no question. Somebody who does have an opinion — a second drive, say —
 * finds the old picker under "Advanced".
 */
function JoinDataDirStep({ onNext }: { onNext: (dataDir: string) => void }) {
  const [defaultDir, setDefaultDir] = useState("");
  const [customDir, setCustomDir] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void invoke<string>("default_player_data_directory")
      .then((dir) => {
        if (!cancelled) setDefaultDir(dir);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(String(err));
        setAdvanced(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const dataDir = advanced && customDir.length > 0 ? customDir : defaultDir;

  const handleBrowse = async () => {
    setError(null);
    try {
      const path = await invoke<string>("open_directory_picker");
      setCustomDir(path);
    } catch {
      setError("No folder selected.");
    }
  };

  const handleContinue = () => {
    setSaving(true);
    setError(null);
    invoke<void>("set_root_path", { path: dataDir })
      .then(() => onNext(dataDir))
      .catch((err) => setError(String(err)))
      .finally(() => setSaving(false));
  };

  return (
    <OnboardingStepCard>
      <CardHeader className="border-b border-border/50 px-6 pb-4 pt-6">
        <div className="mb-3 flex size-10 items-center justify-center rounded-lg border border-accent/40 bg-accent/10 text-accent">
          <FolderOpen className="size-5" />
        </div>
        <CardTitle className="font-display text-base font-bold tracking-[0.2em] text-accent uppercase">
          Where Nerevar keeps things
        </CardTitle>
        <CardDescription className="font-serif text-base font-light tracking-[0.05em] leading-relaxed text-foreground/75">
          The server's mods and its copy of TES3MP are downloaded here. It can
          grow to several gigabytes.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4 px-6 py-5">
        <p className="rounded-lg border border-border/50 bg-background/30 p-3 font-mono text-xs break-all text-accent">
          {dataDir || "..."}
        </p>

        {advanced ? (
          <div className="space-y-2">
            <Label
              htmlFor="join-data-dir"
              className="font-display text-[0.75rem] tracking-[0.3em] uppercase text-foreground/70"
            >
              Data directory
            </Label>
            <div className="flex gap-2">
              <Input
                id="join-data-dir"
                readOnly
                placeholder={exampleDataDirPath()}
                value={customDir}
                className="font-mono text-xs bg-input/40 truncate"
              />
              <Button
                type="button"
                variant="outline"
                className="shrink-0 font-display text-[0.75rem] tracking-[0.3em] uppercase"
                onClick={() => void handleBrowse()}
              >
                Browse
              </Button>
            </div>
          </div>
        ) : (
          <button
            type="button"
            className="flex w-fit items-center gap-2 font-display text-[0.7rem] tracking-[0.25em] text-foreground/60 uppercase hover:text-accent"
            onClick={() => setAdvanced(true)}
          >
            <Settings2 className="size-3.5" />
            Advanced: choose another folder
          </button>
        )}

        {error ? <StepError message={error} /> : null}
      </CardContent>
      <StepActions
        onPrimary={handleContinue}
        primaryLabel="Continue"
        showBack={false}
        nextDisabled={saving || dataDir.length === 0}
      />
    </OnboardingStepCard>
  );
}

const JOIN_GUIDE_POINTS = [
  {
    icon: Download,
    title: "The mods arrive on their own",
    body: "Your friend's server decides the mod list. Nerevar downloads it the first time you connect and pulls only what changed after that.",
  },
  {
    icon: Play,
    title: "Launch from the server's page",
    body: "Press Launch client. Nerevar checks for updates, syncs anything new, and starts TES3MP with the right load order.",
  },
  {
    icon: ShieldCheck,
    title: "Your own game is left alone",
    body: "Your Morrowind and your OpenMW configuration are never modified — Nerevar puts its own configuration in place while you play and restores yours afterwards.",
  },
];

/** Step 3: the short version of what playing on a synced server means. */
function JoinGuideStep({
  onNext,
  onBack,
}: {
  onNext: () => void;
  onBack: () => void;
}) {
  return (
    <OnboardingStepCard>
      <CardHeader className="border-b border-border/50 px-6 pb-4 pt-6">
        <CardTitle className="font-display text-base font-bold tracking-[0.2em] text-accent uppercase">
          How playing works
        </CardTitle>
        <CardDescription className="font-serif text-base font-light tracking-[0.05em] leading-relaxed text-foreground/75">
          Three things worth knowing before you connect.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-2 px-6 py-5">
        {JOIN_GUIDE_POINTS.map((point) => (
          <div
            key={point.title}
            className="flex gap-3 rounded-lg border border-border/50 bg-background/30 px-3 py-3"
          >
            <div className="flex size-8 shrink-0 items-center justify-center rounded-md border border-accent/30 text-accent/80">
              <point.icon className="size-4" />
            </div>
            <div>
              <p className="font-display text-sm tracking-[0.12em] text-accent uppercase">
                {point.title}
              </p>
              <p className="mt-1 font-serif text-sm leading-relaxed text-foreground/70">
                {point.body}
              </p>
            </div>
          </div>
        ))}
      </CardContent>
      <StepActions onBack={onBack} onPrimary={onNext} primaryLabel="Continue" />
    </OnboardingStepCard>
  );
}

/**
 * Step 4: the address, and the join itself.
 *
 * "Connect" pings the host and reads its manifest summary, which is what names
 * the server on screen and preselects the TES3MP runtime the host advertises.
 * The picker appears with that preselection and the second press installs it
 * and creates the instance — the New Connection page's two steps, with the
 * name, description and port fields dropped because the summary already
 * carries them.
 *
 * Why two presses and not one: which TES3MP build a player ends up running is
 * still the owner's-choice question nobody has ruled on, so the build is shown
 * and confirmed rather than installed on the player's behalf.
 */
function JoinConnectStep({
  dataDir,
  onBack,
  onJoined,
}: {
  dataDir: string;
  onBack: () => void;
  onJoined: (instanceId: string) => void;
}) {
  const config = useConfig();
  const { runOperation } = useBackgroundOperation();
  const [host, setHost] = useState("");
  const [password, setPassword] = useState("");
  const [summary, setSummary] = useState<RemoteManifestSummary | null>(null);
  const [runtime, setRuntime] = useState<RuntimeSource>({
    ...emptyRuntimeSource,
  });
  const [suggestion, setSuggestion] = useState<HostRuntimeHint | null>(null);
  // A local runtime the backend would reject: the join button stays disabled
  // rather than letting the create fail after the copy.
  const [runtimeBlocked, setRuntimeBlocked] = useState(false);
  const [phase, setPhase] = useState<"idle" | "connecting" | "joining">("idle");
  const [error, setError] = useState<string | null>(null);

  const busy = phase !== "idle";
  const syncPort = config?.syncPort ?? 25567;

  const join = async (
    remoteSummary: RemoteManifestSummary,
    chosenRuntime: RuntimeSource,
  ) => {
    setPhase("joining");
    const connectionName = await invoke<string>("unique_synced_instance_name", {
      desiredName: remoteSummary.instanceName,
    });
    const payload: NewConnectionConfig = {
      runtime: chosenRuntime,
      connectionName,
      connectionDescription: "",
      // Just the Nerevar data directory: the command derives the instance's
      // own paths from it and the connection name.
      rootPath: dataDir,
      remoteHost: host.trim(),
      remoteSyncPort: syncPort,
      syncPassword: password,
    };

    // Through `runOperation` so the runtime install's progress lands on the
    // banner, the same way the New Connection page reports it.
    const instanceId = await runOperation({
      instanceId: "",
      instanceName: connectionName,
      kind: "createConnection",
      detail: "Installing the TES3MP runtime",
      task: (operationId) =>
        invoke<string>("add_synced_connection", {
          newConnection: payload,
          operationId,
        }),
    });
    onJoined(instanceId);
  };

  const handleConnect = async () => {
    const addressError = hostAddressError(host);
    if (addressError) {
      setError(addressError);
      return;
    }

    setError(null);
    setPhase("connecting");
    try {
      const remoteHost = host.trim();
      await invoke("ping_remote_nerevar_server", {
        remoteHost,
        remoteSyncPort: syncPort,
      });
      const remoteSummary = await invoke<RemoteManifestSummary>(
        "fetch_remote_manifest_summary",
        {
          remoteHost,
          remoteSyncPort: syncPort,
          syncPassword: password || null,
        },
      );
      setSummary(remoteSummary);

      const hint = await resolveHostRuntimeHint(remoteSummary);
      if (hint) {
        // An untrusted suggestion sets the field to the official repository
        // with no release chosen; only a trusted one preselects the host's.
        setRuntime(hint.runtime);
        setSuggestion(hint);
      } else {
        setSuggestion(null);
      }
      setPhase("idle");
    } catch (err) {
      setError(String(err));
      setPhase("idle");
    }
  };

  const handleJoin = async () => {
    if (!summary) return;
    setError(null);
    try {
      await join(summary, runtime);
    } catch (err) {
      setError(String(err));
      setPhase("idle");
    }
  };

  /** A typed-over address is a different server: connect to it again. */
  const handleHostChange = (value: string) => {
    setHost(value);
    if (summary) {
      setSummary(null);
      setSuggestion(null);
    }
  };

  return (
    <OnboardingStepCard>
      <CardHeader className="border-b border-border/50 px-6 pb-4 pt-6">
        <div className="mb-3 flex size-10 items-center justify-center rounded-lg border border-accent/40 bg-accent/10 text-accent">
          <Plug className="size-5" />
        </div>
        <CardTitle className="font-display text-base font-bold tracking-[0.2em] text-accent uppercase">
          Join the server
        </CardTitle>
        <CardDescription className="font-serif text-base font-light tracking-[0.05em] leading-relaxed text-foreground/75">
          Everything else — the server's name, its mods, its game port — Nerevar
          reads from the server itself.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4 px-6 py-5">
        <div className="space-y-2">
          <Label
            htmlFor="join-host"
            className="font-display text-[0.75rem] tracking-[0.3em] uppercase text-foreground/70"
          >
            Host address, as your friend gave it
          </Label>
          <Input
            id="join-host"
            value={host}
            disabled={busy}
            autoComplete="off"
            placeholder="mw.example.org"
            onChange={(event) => handleHostChange(event.target.value)}
          />
        </div>

        <div className="space-y-2">
          <Label
            htmlFor="join-password"
            className="font-display text-[0.75rem] tracking-[0.3em] uppercase text-foreground/70"
          >
            Password (only if they gave you one)
          </Label>
          <Input
            id="join-password"
            type="password"
            autoComplete="off"
            value={password}
            disabled={busy}
            onChange={(event) => setPassword(event.target.value)}
          />
        </div>

        {summary ? (
          <div className="space-y-1 rounded-lg border border-accent/30 bg-accent/5 p-4">
            <p className="font-display text-xs tracking-[0.15em] text-accent uppercase">
              {summary.instanceName}
            </p>
            <p className="font-serif text-sm text-foreground/80">
              {summary.packageCount} package(s),{" "}
              {formatByteSize(summary.totalDownloadBytes)} to download
            </p>
          </div>
        ) : null}

        {summary ? (
          <div className="space-y-2">
            <Label className="font-display text-[0.75rem] tracking-[0.3em] uppercase text-foreground/70">
              TES3MP version
            </Label>
            <p
              className={
                suggestion && !suggestion.trusted
                  ? "font-serif text-sm text-destructive"
                  : "font-serif text-sm text-foreground/70"
              }
            >
              {suggestion
                ? suggestion.notice
                : "This server does not say which TES3MP build it runs. Pick the release to install."}
            </p>
            <RuntimeSourceField
              value={runtime}
              onValueChange={setRuntime}
              disabled={busy}
              onBlockingChange={setRuntimeBlocked}
            />
          </div>
        ) : null}

        {error ? <StepError message={error} /> : null}
      </CardContent>
      <StepActions
        onBack={onBack}
        onPrimary={() => (summary ? void handleJoin() : void handleConnect())}
        primaryLabel={
          phase === "joining"
            ? "Setting up"
            : phase === "connecting"
              ? "Connecting"
              : summary
                ? "Download & join"
                : "Connect"
        }
        nextDisabled={
          busy ||
          host.trim().length === 0 ||
          (summary !== null &&
            (runtimeBlocked || !runtimeSourceSchema.safeParse(runtime).success))
        }
      />
    </OnboardingStepCard>
  );
}

/** Whatever the backend said went wrong, in its own words. */
function StepError({ message }: { message: string }) {
  return (
    <p className="rounded-lg border border-destructive/40 bg-destructive/10 p-3 font-serif text-sm text-destructive">
      {message}
    </p>
  );
}
