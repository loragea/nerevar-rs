import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { normalizeRepo } from "@/features/instances/schemas/runtime-source-schema";
import { useTrustedRuntimeRepos } from "@/features/settings/hooks/use-trusted-runtime-repos";
import type { TrustedRepo } from "@/types";
import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, Loader2, Plus, ShieldCheck, X } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";

/**
 * The trusted runtime sources: which GitHub repositories Nerevar may download
 * a TES3MP build from.
 *
 * A server tells its players which *version* to run and never which
 * repository, so this list is the only thing that widens where an executable
 * can come from — and adding to it is deliberate, typed out by hand, and
 * warned about. Removing a built-in is not offered: it is what the app itself
 * vouches for.
 */
export function TrustedRuntimeSourcesSection() {
  const { repos, setRepos } = useTrustedRuntimeRepos();
  const [candidate, setCandidate] = useState("");
  const [busy, setBusy] = useState(false);

  const normalized = normalizeRepo(candidate);

  const addRepo = async () => {
    setBusy(true);
    try {
      const updated = await invoke<TrustedRepo[]>("add_trusted_runtime_repo", {
        repo: candidate,
      });
      setRepos(updated);
      setCandidate("");
      toast.success(`${normalized} added to your trusted runtime sources`);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  const removeRepo = async (repo: string) => {
    setBusy(true);
    try {
      const updated = await invoke<TrustedRepo[]>(
        "remove_trusted_runtime_repo",
        { repo },
      );
      setRepos(updated);
      toast.success(`${repo} removed from your trusted runtime sources`);
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="font-display text-lg tracking-[0.15em] text-accent uppercase">
          Trusted runtime sources
        </h2>
        <ShieldCheck className="size-4 shrink-0 text-accent/70" />
      </div>
      <p className="font-serif text-sm leading-relaxed text-foreground/70">
        The GitHub repositories Nerevar may download a TES3MP build from. A
        server can ask its players for a particular TES3MP version, but never
        for a repository: only this list decides where a build comes from.
      </p>

      <ul className="flex flex-col gap-2">
        {(repos ?? []).map((entry) => (
          <li
            key={entry.repo}
            className="flex items-center justify-between gap-2 rounded-lg border border-border/50 bg-background/30 px-3 py-2"
          >
            <span className="truncate font-mono text-xs text-foreground/80">
              {entry.repo}
            </span>
            {entry.builtin ? (
              <Badge variant="secondary" className="font-mono text-[0.65rem]">
                Built in
              </Badge>
            ) : (
              <Button
                variant="ghost"
                size="sm"
                className="shrink-0 text-foreground/60 hover:text-destructive"
                disabled={busy}
                onClick={() => void removeRepo(entry.repo)}
                aria-label={`Remove ${entry.repo}`}
              >
                <X className="size-4" />
                Remove
              </Button>
            )}
          </li>
        ))}
      </ul>

      <div className="space-y-2">
        <Label
          htmlFor="trusted-runtime-repo"
          className="font-display text-[0.75rem] tracking-[0.2em] uppercase text-foreground/70"
        >
          Add repository
        </Label>
        <p className="flex items-start gap-2 font-serif text-sm leading-relaxed text-destructive">
          <AlertTriangle className="mt-0.5 size-4 shrink-0" />
          <span>
            Nerevar downloads and runs executables from a trusted source. Add a
            repository only if you trust whoever publishes its releases with
            your machine.
          </span>
        </p>
        <div className="flex gap-2">
          <Input
            id="trusted-runtime-repo"
            value={candidate}
            spellCheck={false}
            autoCapitalize="none"
            autoCorrect="off"
            placeholder="owner/name"
            disabled={busy}
            className="font-mono text-xs"
            onChange={(event) => setCandidate(event.target.value)}
          />
          <Button
            type="button"
            variant="outline"
            className="shrink-0 font-display text-[0.75rem] tracking-[0.3em] uppercase"
            disabled={busy || normalized === null}
            onClick={() => void addRepo()}
          >
            {busy ? (
              <Loader2 className="animate-spin" data-icon="inline-start" />
            ) : (
              <Plus data-icon="inline-start" />
            )}
            Add repository
          </Button>
        </div>
        {candidate.trim().length > 0 && normalized === null ? (
          <p className="font-serif text-sm text-destructive">
            Enter the repository as owner/name.
          </p>
        ) : null}
      </div>
    </section>
  );
}
