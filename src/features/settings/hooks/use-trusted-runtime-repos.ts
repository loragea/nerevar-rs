import { useConfig } from "@/features/config/context/config-context-provider";
import type { TrustedRepo } from "@/types";
import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useState } from "react";

/**
 * The GitHub repositories Nerevar is allowed to download a TES3MP runtime
 * from: the built-in list plus whatever this player added under Settings.
 *
 * Read from the backend rather than from `config.trustedRuntimeRepos`,
 * because the built-in entries live in the Rust source and never appear in
 * the config file. The list re-reads whenever the config changes, so a repo
 * added in Settings is trusted everywhere in the app immediately.
 */
export function useTrustedRuntimeRepos() {
  const config = useConfig();
  const [repos, setRepos] = useState<TrustedRepo[] | null>(null);

  const refresh = useCallback(async () => {
    try {
      setRepos(await invoke<TrustedRepo[]>("get_trusted_runtime_repos"));
    } catch {
      // Nothing to show; the backend refuses an untrusted download anyway.
      setRepos(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh, config?.trustedRuntimeRepos]);

  return { repos, refresh, setRepos };
}

/**
 * Whether `repo` is in `repos`, comparing the way the backend does: GitHub
 * account and repository names are case-insensitive. `null` (the list has not
 * loaded) is treated as trusted so the UI never accuses a repository on the
 * strength of a failed read — the backend is what actually refuses.
 */
export function isRepoTrusted(
  repos: TrustedRepo[] | null,
  repo: string,
): boolean {
  if (repos === null) return true;
  const wanted = repo.trim().toLowerCase();
  return repos.some((entry) => entry.repo.toLowerCase() === wanted);
}
