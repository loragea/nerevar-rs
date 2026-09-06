import { z } from "zod";

/**
 * The address of a Nerevar sync host, as typed into a connection form.
 *
 * Mirrors `nerevar-core`'s `sync_client::host_address::base_url`, which is the
 * authority: a bare hostname, IP, or bracketed IPv6 literal is combined with the
 * configured sync port as `http://{host}:{port}`, while a full `http://` or
 * `https://` URL — the form a host behind a reverse proxy takes — is used as the
 * base as given, with its own port and any path prefix, and the sync port is
 * ignored. Anything else is rejected here so the user hears about it before a
 * request is attempted.
 */

export const HOST_ADDRESS_HELP =
  "Hostname or IP, or a full http(s):// URL when the host is behind a reverse proxy (then the sync port is ignored).";

/** True when the address is a full URL, so the configured sync port is unused. */
export function isUrlHostAddress(host: string): boolean {
  return stripTrailingSlashes(host.trim()).includes("://");
}

/** The validation message for `value`, or `null` when it is a usable address. */
export function hostAddressError(value: string): string | null {
  const host = value.trim();
  if (host.length === 0) {
    return "Host address is required";
  }
  if (/\s/.test(host)) {
    return "Host address cannot contain spaces";
  }

  const trimmed = stripTrailingSlashes(host);
  if (trimmed.length === 0) {
    return "Host address is required";
  }

  const schemeEnd = trimmed.indexOf("://");
  if (schemeEnd >= 0) {
    const scheme = trimmed.slice(0, schemeEnd).toLowerCase();
    if (scheme !== "http" && scheme !== "https") {
      return `Only http:// and https:// addresses are supported (got "${scheme}://")`;
    }
    if (trimmed.slice(schemeEnd + 3).length === 0) {
      return "Enter a host after the scheme, e.g. https://mw.example.org";
    }
    return null;
  }

  if (trimmed.includes("/")) {
    return "Enter just the hostname or IP, or a full http(s):// URL";
  }
  return null;
}

/** The shared zod field for a sync host address. */
export const hostAddressSchema = z
  .string()
  .trim()
  .max(253, "Host is too long")
  .superRefine((value, ctx) => {
    const message = hostAddressError(value);
    if (message) {
      ctx.addIssue({ code: "custom", message });
    }
  });

function stripTrailingSlashes(value: string): string {
  return value.replace(/\/+$/, "");
}
