import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useAppUpdate } from "@/features/updates/context/update-context-provider";
import { cn } from "@/lib/utils";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  ArrowDown,
  ArrowLeft,
  Download,
  ExternalLink,
  Loader2,
  Sparkles,
} from "lucide-react";
import { useState } from "react";
import { Link, Redirect } from "wouter";
import { toast } from "sonner";

const cardClass =
  "gap-0 border-border/80 bg-card/70 py-0 shadow-[0_0_20px_hsl(var(--accent)/0.12)] ring-1 ring-accent/30";

function ReleaseNotes({ body }: { body: string }) {
  return (
    <div className="space-y-3 font-serif text-sm leading-relaxed text-foreground/80">
      {body.split("\n").map((line, index) => {
        const trimmed = line.trim();
        if (!trimmed) {
          return <div key={index} className="h-2" />;
        }
        if (trimmed.startsWith("## ")) {
          return (
            <h3
              key={index}
              className="pt-2 font-display text-sm tracking-[0.12em] text-accent uppercase"
            >
              {trimmed.slice(3)}
            </h3>
          );
        }
        if (trimmed.startsWith("- ")) {
          return (
            <p key={index} className="pl-4 before:mr-2 before:content-['•']">
              {trimmed.slice(2)}
            </p>
          );
        }
        return <p key={index}>{trimmed}</p>;
      })}
    </div>
  );
}

export function UpdateAvailablePage() {
  const { status, loading } = useAppUpdate();
  const [installing, setInstalling] = useState(false);

  if (!loading && status && !status.updateAvailable) {
    return <Redirect to="/" />;
  }

  const release = status?.latestRelease;
  const currentVersion = status?.currentVersion ?? "…";
  const nextVersion = release?.version ?? "…";
  // The backend decides which install story this platform has: Windows
  // downloads and runs the installer, everywhere else the distro package (or
  // the AppImage) owns the install and we only point at the release page.
  const selfInstalls = release?.action === "runInstaller";

  const handleInstall = async () => {
    if (!release) return;
    setInstalling(true);
    try {
      await invoke("download_and_run_nerevar_update", { releaseId: release.id });
    } catch (error) {
      toast.error(String(error));
      setInstalling(false);
    }
  };

  const handleOpenReleasePage = async () => {
    if (!release) return;
    try {
      await openUrl(release.htmlUrl);
    } catch (error) {
      toast.error(String(error));
    }
  };

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-5 px-1 pb-10">
      <Button
        variant="outline"
        size="sm"
        className="w-fit font-display text-sm tracking-[0.2em] text-foreground/70 uppercase hover:text-accent"
        asChild
      >
        <Link href="/">
          <ArrowLeft data-icon="inline-start" />
          Dashboard
        </Link>
      </Button>

      <Card className={cn(cardClass, "overflow-hidden")}>
        <CardHeader className="border-b border-border/50 space-y-4 px-6 pb-5 pt-6">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="space-y-2">
              <div className="flex items-center gap-2 text-accent">
                <Sparkles className="size-5" />
                <CardTitle className="font-display text-3xl tracking-[0.08em] text-gradient-gold">
                  Update available
                </CardTitle>
              </div>
              <CardDescription className="max-w-xl font-serif text-base leading-relaxed text-foreground/75">
                {selfInstalls
                  ? "A newer Nerevar release is ready. Review the notes below, then install the update. Nerevar will download the installer, launch it, and close so you can finish upgrading."
                  : "A newer Nerevar release is ready. Review the notes below, then update Nerevar the way you installed it — through your package manager, or from the downloads on the release page."}
              </CardDescription>
            </div>
            <Badge
              variant="outline"
              className="border-accent/40 px-3 py-1 text-accent"
            >
              v{currentVersion} → v{nextVersion}
            </Badge>
          </div>

          <div className="grid gap-3 sm:grid-cols-2">
            <div className="rounded-lg border border-border/50 bg-background/20 px-4 py-3">
              <p className="font-display text-xs tracking-[0.15em] text-foreground/55 uppercase">
                Current version
              </p>
              <p className="mt-1 font-mono text-lg text-foreground">
                v{currentVersion}
              </p>
            </div>
            <div className="rounded-lg border border-accent/30 bg-accent/5 px-4 py-3">
              <p className="font-display text-xs tracking-[0.15em] text-accent uppercase">
                Update to
              </p>
              <p className="mt-1 font-mono text-lg text-accent">
                v{nextVersion}
              </p>
              {release ? (
                <p className="mt-1 font-serif text-sm text-foreground/65">
                  {release.name}
                </p>
              ) : null}
            </div>
          </div>
        </CardHeader>

        <CardContent className="space-y-5 px-6 py-5">
          {loading || !release ? (
            <div className="flex min-h-[240px] items-center justify-center">
              <Loader2 className="size-6 animate-spin text-accent" />
            </div>
          ) : (
            <>
              <div className="flex items-center gap-2 text-foreground/60">
                <ArrowDown className="size-4 text-accent" />
                <p className="font-display text-xs tracking-[0.15em] uppercase">
                  Release notes
                </p>
              </div>
              <div className="max-h-[min(50vh,520px)] overflow-y-auto rounded-lg border border-border/40 bg-background/20 p-4">
                {release.body.trim() ? (
                  <ReleaseNotes body={release.body} />
                ) : (
                  <p className="font-serif text-sm text-foreground/65">
                    No release notes were provided for this version.
                  </p>
                )}
              </div>

              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <Button variant="outline" asChild>
                  <a
                    href={release.htmlUrl}
                    target="_blank"
                    rel="noreferrer"
                  >
                    <ExternalLink data-icon="inline-start" />
                    View on GitHub
                  </a>
                </Button>
                {selfInstalls ? (
                  <Button
                    variant="launch"
                    className="min-w-[220px]"
                    disabled={installing}
                    onClick={() => void handleInstall()}
                  >
                    {installing ? (
                      <Loader2
                        className="animate-spin"
                        data-icon="inline-start"
                      />
                    ) : (
                      <Download data-icon="inline-start" />
                    )}
                    {installing
                      ? "Preparing installer…"
                      : "Download and install"}
                  </Button>
                ) : (
                  <Button
                    variant="launch"
                    className="min-w-[220px]"
                    onClick={() => void handleOpenReleasePage()}
                  >
                    <ExternalLink data-icon="inline-start" />
                    Open the release page
                  </Button>
                )}
              </div>
              {selfInstalls && release.installerAssetName ? (
                <p className="font-serif text-sm text-foreground/60">
                  Installer: <code>{release.installerAssetName}</code>
                </p>
              ) : (
                <p className="font-serif text-sm text-foreground/60">
                  Installed from a distribution package? Update it there
                  (<code>apt</code>, <code>dnf</code> or <code>pacman</code>)
                  rather than from the release page — see{" "}
                  <code>docs/installing.md</code>.
                </p>
              )}
            </>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
