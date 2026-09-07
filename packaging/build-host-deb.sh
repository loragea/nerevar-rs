#!/usr/bin/env bash
#
# WHAT: Builds the `nerevar-host` Debian package — the nerevar-host daemon and
#       the nerevar-cli client under /usr/bin, the systemd unit (installed
#       disabled), /etc/nerevar/ for the config, and the operator guide as the
#       package doc. Uses only dpkg-deb and coreutils; no cargo-deb, no
#       debhelper, no Node or Tauri toolchain.
#
# WHEN: Cutting a release (the release workflow calls this), or checking by
#       hand what the daemon package would contain. This is the only procedure
#       for producing that .deb — do not assemble one by hand.
#
# USAGE:
#   packaging/build-host-deb.sh [options]
#
#     --version <v>   Package version. Default: [workspace.package] version in
#                     src-tauri/Cargo.toml.
#     --arch <a>      Debian architecture. Default: `dpkg --print-architecture`.
#     --bin-dir <d>   Directory holding prebuilt `nerevar-host` and
#                     `nerevar-cli`. Default: build them with
#                     `cargo build --release`.
#     --out-dir <d>   Where the .deb lands. Default: <repo>/target/package.
#     -h, --help      This header.
#
#   Honours CARGO_TARGET_DIR for the cargo build it may run.
#
#   Output: <out-dir>/nerevar-host_<version>_<arch>.deb
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGING_DIR="$REPO_ROOT/packaging"

VERSION=""
ARCH=""
BIN_DIR=""
OUT_DIR=""

usage() {
	sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//;$d'
}

while [ $# -gt 0 ]; do
	case "$1" in
	--version)
		VERSION="$2"
		shift 2
		;;
	--arch)
		ARCH="$2"
		shift 2
		;;
	--bin-dir)
		BIN_DIR="$2"
		shift 2
		;;
	--out-dir)
		OUT_DIR="$2"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	esac
done

command -v dpkg-deb >/dev/null || {
	echo "dpkg-deb not found; install the dpkg-dev package" >&2
	exit 1
}

if [ -z "$VERSION" ]; then
	# The one place the version lives for the Rust side: [workspace.package]
	# in src-tauri/Cargo.toml, which every crate inherits.
	VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/p' "$REPO_ROOT/src-tauri/Cargo.toml" |
		sed -n 's/^version *= *"\(.*\)"/\1/p' | head -1)"
fi
[ -n "$VERSION" ] || {
	echo "could not determine the version; pass --version" >&2
	exit 1
}

if [ -z "$ARCH" ]; then
	ARCH="$(dpkg --print-architecture)"
fi

if [ -z "$OUT_DIR" ]; then
	OUT_DIR="$REPO_ROOT/target/package"
fi

if [ -z "$BIN_DIR" ]; then
	echo "==> cargo build --release -p nerevar-host -p nerevar-cli"
	(cd "$REPO_ROOT/src-tauri" && cargo build --release -p nerevar-host -p nerevar-cli)
	BIN_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/src-tauri/target}/release"
fi

for bin in nerevar-host nerevar-cli; do
	[ -x "$BIN_DIR/$bin" ] || {
		echo "missing binary: $BIN_DIR/$bin" >&2
		exit 1
	}
done

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/nerevar-host-deb.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

echo "==> staging nerevar-host $VERSION ($ARCH) in $STAGE"

install -d -m 0755 "$STAGE/DEBIAN"
install -d -m 0755 "$STAGE/usr/bin"
install -d -m 0755 "$STAGE/lib/systemd/system"
install -d -m 0755 "$STAGE/etc/nerevar"
install -d -m 0755 "$STAGE/usr/share/doc/nerevar-host"

install -m 0755 "$BIN_DIR/nerevar-host" "$STAGE/usr/bin/nerevar-host"
install -m 0755 "$BIN_DIR/nerevar-cli" "$STAGE/usr/bin/nerevar-cli"
# Release binaries carry debug info by default; a package does not need it.
if command -v strip >/dev/null 2>&1; then
	strip --strip-unneeded "$STAGE/usr/bin/nerevar-host" "$STAGE/usr/bin/nerevar-cli"
fi

# The checked-in unit is the from-source example, so its ExecStart points at
# /usr/local/bin (where `sudo install` puts a hand-built binary). The package
# owns /usr/bin, so rewrite the path — and refuse to build a package whose unit
# would point at a binary that is not in it.
sed 's,/usr/local/bin/nerevar-host,/usr/bin/nerevar-host,g' \
	"$PACKAGING_DIR/systemd/nerevar-host.service" \
	>"$STAGE/lib/systemd/system/nerevar-host.service"
chmod 0644 "$STAGE/lib/systemd/system/nerevar-host.service"
grep -q '^ExecStart=/usr/bin/nerevar-host ' "$STAGE/lib/systemd/system/nerevar-host.service" || {
	echo "staged unit has no ExecStart=/usr/bin/nerevar-host line" >&2
	exit 1
}

install -m 0644 "$REPO_ROOT/docs/headless-hosting.md" \
	"$STAGE/usr/share/doc/nerevar-host/headless-hosting.md"
install -m 0644 "$PACKAGING_DIR/debian/copyright" \
	"$STAGE/usr/share/doc/nerevar-host/copyright"

# dpkg reports Installed-Size in KiB.
INSTALLED_SIZE="$(du -ks --exclude=DEBIAN "$STAGE" | cut -f1)"

sed -e "s/@VERSION@/$VERSION/" \
	-e "s/@ARCH@/$ARCH/" \
	-e "s/@INSTALLED_SIZE@/$INSTALLED_SIZE/" \
	"$PACKAGING_DIR/debian/control.in" >"$STAGE/DEBIAN/control"

for script in postinst prerm postrm; do
	install -m 0755 "$PACKAGING_DIR/debian/$script" "$STAGE/DEBIAN/$script"
done

# No DEBIAN/conffiles: /etc/nerevar is an empty directory the package owns, and
# the daemon neither ships nor creates a config.json to protect on upgrade.

# Everything ships root-owned. `dpkg-deb --root-owner-group` avoids needing
# fakeroot for that, so this runs unprivileged in CI.
mkdir -p "$OUT_DIR"
DEB="$OUT_DIR/nerevar-host_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$STAGE" "$DEB" >/dev/null

echo "==> $DEB"
dpkg-deb --info "$DEB" | sed 's/^/    /'
