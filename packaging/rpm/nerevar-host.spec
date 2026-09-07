## RPM package for the headless Nerevar host daemon and the command-line
## client — the same contents packaging/build-host-deb.sh puts in the .deb:
## /usr/bin/nerevar-host, /usr/bin/nerevar-cli, a systemd unit that ships
## disabled, /etc/nerevar/ for the config, and the operator guide.
##
## Build from a release tarball of the fork's tag:
##   spectool -g -R packaging/rpm/nerevar-host.spec
##   rpmbuild -ba packaging/rpm/nerevar-host.spec
##
## Needs the Rust toolchain and network access for `cargo build` (crates.io);
## for a fully offline build, vendor the crates first and drop a
## .cargo/config.toml pointing at the vendor directory into %{_builddir}.

Name:           nerevar-host
Version:        0.2.0
Release:        1%{?dist}
Summary:        Headless TES3MP host daemon and command-line Nerevar client

License:        GPL-3.0-only
URL:            https://github.com/loragea/nerevar-rs
Source0:        %{url}/archive/v%{version}/nerevar-rs-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  systemd-rpm-macros

Requires(pre):  shadow-utils
%{?systemd_requires}

%description
nerevar-host hosts one owned Nerevar instance on a machine with no desktop: it
rebuilds the instance's mod manifest, serves it and the mod files to Nerevar
clients over HTTP (or HTTPS), and runs the instance's TES3MP dedicated server.
It logs to stderr, exits on SIGTERM, and expects a service manager to own it;
the systemd unit in this package ships disabled.

nerevar-cli is the matching command-line client: "nerevar-cli sync" is headless
player-side sync for a Linux box with no desktop, and "nerevar-cli admin"
drives a headless host's /admin routes as a co-admin over a bearer token.

This package contains no Morrowind or TES3MP game data.

%prep
%autosetup -n nerevar-rs-%{version}

%build
cd src-tauri
cargo build --release -p nerevar-host -p nerevar-cli

%install
install -D -m 0755 src-tauri/target/release/nerevar-host %{buildroot}%{_bindir}/nerevar-host
install -D -m 0755 src-tauri/target/release/nerevar-cli  %{buildroot}%{_bindir}/nerevar-cli

# Same rewrite the .deb build does: the checked-in unit is the from-source
# example and points at /usr/local/bin, while the package owns %{_bindir}.
sed 's,/usr/local/bin/nerevar-host,%{_bindir}/nerevar-host,g' \
    packaging/systemd/nerevar-host.service \
    > %{buildroot}/nerevar-host.service.tmp
install -D -m 0644 %{buildroot}/nerevar-host.service.tmp \
    %{buildroot}%{_unitdir}/nerevar-host.service
rm -f %{buildroot}/nerevar-host.service.tmp

install -d -m 0750 %{buildroot}%{_sysconfdir}/nerevar
install -D -m 0644 docs/headless-hosting.md \
    %{buildroot}%{_docdir}/%{name}/headless-hosting.md

%pre
# The service account the unit runs as; its home is where
# docs/headless-hosting.md puts the instance trees.
getent group nerevar >/dev/null || groupadd -r nerevar
getent passwd nerevar >/dev/null || \
    useradd -r -g nerevar -d /srv/nerevar -s /sbin/nologin \
            -c "Nerevar headless TES3MP host" nerevar
exit 0

%post
# Registers the unit with systemd without enabling it: an operator has to write
# /etc/nerevar/config.json and set an instance up before starting anything.
%systemd_post nerevar-host.service
if [ ! -d /srv/nerevar ]; then
    mkdir -p /srv/nerevar
fi
chown nerevar:nerevar /srv/nerevar
chmod 0755 /srv/nerevar

%preun
%systemd_preun nerevar-host.service

%postun
%systemd_postun_with_restart nerevar-host.service

%files
%license LICENSE
%{_bindir}/nerevar-host
%{_bindir}/nerevar-cli
%{_unitdir}/nerevar-host.service
%dir %attr(0750, root, nerevar) %{_sysconfdir}/nerevar
%dir %{_docdir}/%{name}
%{_docdir}/%{name}/headless-hosting.md
