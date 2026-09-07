# Installing Nerevar

Nerevar ships as native packages for the desktop app and as a separate package
for the headless host daemon. Every download lives on the
[releases page](https://github.com/loragea/nerevar-rs/releases).

- [Which package do I want?](#which-package-do-i-want)
- [Debian and Ubuntu](#debian-and-ubuntu)
- [Fedora and other rpm distributions](#fedora-and-other-rpm-distributions)
- [Arch Linux](#arch-linux)
- [AppImage](#appimage)
- [Windows](#windows)
- [Where your files live](#where-your-files-live)
- [How updates arrive](#how-updates-arrive)
- [What Nerevar does not install](#what-nerevar-does-not-install)

## Which package do I want?

| You are                                | Take                                             |
| -------------------------------------- | ------------------------------------------------ |
| A player on Debian, Ubuntu, or Mint    | `Nerevar_<version>_amd64.deb`                    |
| A player on Fedora, RHEL, or openSUSE  | `Nerevar-<version>-1.x86_64.rpm`                 |
| A player on Arch                       | `packaging/arch/nerevar/PKGBUILD`                |
| A player on any other Linux            | `Nerevar_<version>_amd64.AppImage`               |
| A player on Windows                    | `Nerevar_<version>_x64-setup.exe`                |
| Running a dedicated server             | `nerevar-host_<version>_amd64.deb`, or the tarball |

The desktop app and the host daemon are separate packages and can be installed
on the same machine; the app is a GUI that also hosts, the daemon is the same
hosting with no GUI. Server operators want
[headless hosting](headless-hosting.md).

## Debian and Ubuntu

Debian 12 or newer, Ubuntu 22.04 or newer — anything with
`libwebkit2gtk-4.1-0`.

```sh
sudo apt install ./Nerevar_<version>_amd64.deb
```

`apt install ./file.deb` (note the `./`) pulls the two runtime dependencies —
`libwebkit2gtk-4.1-0` and `libgtk-3-0` — from the archive. `dpkg -i` does not;
if you use it, follow with `sudo apt -f install`.

Nerevar then appears in your application menu, or runs as `nerevar` from a
terminal.

The daemon package is installed the same way:

```sh
sudo apt install ./nerevar-host_<version>_amd64.deb
```

See [headless hosting](headless-hosting.md#install-the-package) for what it
puts where and what to do next.

## Fedora and other rpm distributions

Fedora 38 or newer; the package requires `webkit2gtk4.1` and `gtk3`.

```sh
sudo dnf install ./Nerevar-<version>-1.x86_64.rpm
```

On openSUSE use `sudo zypper install ./Nerevar-<version>-1.x86_64.rpm`. The
package name for the webview differs between rpm distributions, so if the
install complains about a missing `webkit2gtk4.1`, install your distribution's
WebKitGTK 4.1 package first and retry.

There is no rpm of the daemon on the releases page. Build one from
`packaging/rpm/nerevar-host.spec`, or use the tarball.

## Arch Linux

Two PKGBUILDs live in the repository — one for the desktop app, one for the
daemon. They build from the release tarball of a tag:

```sh
git clone https://github.com/loragea/nerevar-rs.git
cd nerevar-rs/packaging/arch/nerevar
makepkg -si
```

`packaging/arch/nerevar-host/` is the same procedure for the daemon. Neither
is on the AUR.

## AppImage

The lowest-friction route on a distribution nothing else covers: one file, no
package manager, no root.

```sh
chmod +x Nerevar_<version>_amd64.AppImage
./Nerevar_<version>_amd64.AppImage
```

It carries its own copy of the WebKitGTK stack, so it needs no system
dependencies beyond a working desktop. It also does not register itself in your
application menu — that is what the deb and rpm are for.

## Windows

Windows 10 or newer, with the Microsoft Edge WebView2 runtime (already present
on Windows 11 and on any up-to-date Windows 10; the installer fetches it if it
is missing).

Run `Nerevar_<version>_x64-setup.exe`. It installs for the current user, so it
needs no administrator rights.

`Nerevar_<version>_x64_en-US.msi` is the same application as an MSI, for
deployment by policy. Do not use it for a normal install — the in-app updater
only knows how to run the `-setup.exe`.

## Where your files live

Two separate places, and neither is inside the installed package.

**Settings** — `config.json`, which records your instances, your Morrowind
`Data Files` path, and the sync port:

| Platform | Path                                                       |
| -------- | ---------------------------------------------------------- |
| Linux    | `~/.local/share/dev.kyleaustad.nerevar/config.json`         |
| Windows  | `%APPDATA%\dev.kyleaustad.nerevar\config.json`              |
| macOS    | `~/Library/Application Support/dev.kyleaustad.nerevar/config.json` |

`dev.kyleaustad.nerevar` is the application identifier this project inherited
from the repository it was forked from. It stays as it is on purpose: changing
it would move every existing installation's settings out from under it.

**Instances** — the game data itself: each instance's mods, its TES3MP build,
and its saves. You choose the location during first-time setup; the default is
`Nerevar/instances` under the same user data directory:

| Platform | Default                                |
| -------- | -------------------------------------- |
| Linux    | `~/.local/share/Nerevar/instances`     |
| Windows  | `%APPDATA%\Nerevar\instances`          |

These can be large — a heavily modded instance runs to tens of gigabytes — so
if your home directory is on a small disk, point the data directory somewhere
else during setup.

Removing the package leaves both alone. To start over, delete them by hand.

## How updates arrive

Nerevar checks the releases page at startup and tells you when a newer version
exists, but what it offers to do about it depends on how you installed it.

| Installed as       | What happens                                                        |
| ------------------ | ------------------------------------------------------------------- |
| Windows `-setup.exe` | Nerevar downloads the new installer, launches it, and closes.      |
| deb                | Nerevar links you to the release page; install the new `.deb`.       |
| rpm                | Same — download and `dnf install` the new one.                       |
| Arch               | Same — bump `pkgver` in the PKGBUILD and `makepkg -si` again.        |
| AppImage           | Same — download the new AppImage and replace the old file.           |

Only Windows installs itself in place. On Linux the package you installed from
owns the files, so Nerevar refuses to write over them and points at the release
page instead.

The TES3MP runtime updates separately and is not part of this: a server tells
your client which TES3MP version it needs, and Nerevar installs that into the
instance. See [the README](../README.md#trusted-runtime-sources).

## What Nerevar does not install

**Morrowind.** Nerevar needs a legitimate copy of Morrowind — from Steam, GOG,
or a disc — and asks for the path to its `Data Files` directory during setup.
It never downloads or redistributes game data.

**Mods.** Mods come from the server you join, from that server's operator, and
only for servers you choose to sync with. Read the mod-redistribution notice in
[the README](../README.md#important-disclaimer-notice--read-before-hosting-or-sharing-mods)
before hosting or sharing anything.
