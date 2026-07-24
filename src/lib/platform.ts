/**
 * Lightweight platform sniffing for UI display purposes (e.g. example
 * paths/placeholders). Not a substitute for real platform detection when
 * behavior actually depends on the OS — this only informs copy.
 *
 * Under Tauri's WebKitGTK (Linux) and WebView2 (Windows) webviews,
 * `navigator.userAgent` reliably includes the host OS token, so we sniff
 * for "Windows" and default to unix-style paths otherwise.
 */
export function isWindowsPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  return navigator.userAgent.includes("Windows");
}

/** Example Morrowind "Data Files" path, tailored to the current platform. */
export function exampleMorrowindDataFilesPath(): string {
  return isWindowsPlatform()
    ? "C:\\Program Files (x86)\\Steam\\steamapps\\common\\Morrowind\\Data Files"
    : "/path/to/Morrowind/Data Files";
}

/** Example Nerevar data directory path, tailored to the current platform. */
export function exampleDataDirPath(): string {
  return isWindowsPlatform() ? "C:\\Games\\Nerevar" : "/home/you/Games/Nerevar";
}
