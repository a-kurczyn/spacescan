Name:           spacescan
Version:        1.1.0
Release:        1%{?dist}
Summary:        Disk-usage explorer with a sunburst chart, a keyboard-driven table and reports

SourceLicense:  GPL-3.0-only
# Filled in from %%cargo_license_summary (the crates built into the binary).
License:        GPL-3.0-only
URL:            https://github.com/a-kurczyn/spacescan
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
# The crates from Cargo.lock: `cargo vendor --versioned-dirs vendor` in the
# source folder, then `tar -cJf %%{name}-%%{version}-vendor.tar.xz vendor`.
Source1:        %{name}-%{version}-vendor.tar.xz

ExclusiveArch:  %{rust_arches}

BuildRequires:  cargo-rpm-macros >= 26
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Loaded at run time (not linked), so rpm can't find them by itself.
Requires:       libX11
Requires:       libX11-xcb
Requires:       libXcursor
Requires:       libXi
Requires:       libxkbcommon
Requires:       libxkbcommon-x11
Requires:       libwayland-client
Requires:       libwayland-egl
Requires:       libglvnd-egl
Requires:       dbus-libs
Requires:       hicolor-icon-theme
# The folder picker.
Recommends:     xdg-desktop-portal

%description
SpaceScan scans a drive or folder and shows where the space goes, as a
sunburst chart and as a table you can drive from the keyboard. Files are
grouped by kind, with filters by name and category. It can delete, move to
the trash, copy and move files without touching other mounted drives, and
writes text, CSV and JSON reports from the command line.

%prep
%autosetup -n %{name}-%{version} -a1
%cargo_prep -v vendor

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies
%{cargo_vendor_manifest}

%install
install -Dpm 0755 target/rpm/%{name} %{buildroot}%{_bindir}/%{name}
install -Dpm 0644 packaging/%{name}.desktop %{buildroot}%{_datadir}/applications/%{name}.desktop
install -Dpm 0644 packaging/%{name}.png %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/%{name}.png
install -Dpm 0644 packaging/io.github.a_kurczyn.%{name}.metainfo.xml %{buildroot}%{_metainfodir}/io.github.a_kurczyn.%{name}.metainfo.xml

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{name}.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/io.github.a_kurczyn.%{name}.metainfo.xml
%cargo_test

%files
%license LICENSE LICENSE.dependencies cargo-vendor.txt
%doc README.md
%{_bindir}/%{name}
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/256x256/apps/%{name}.png
%{_metainfodir}/io.github.a_kurczyn.%{name}.metainfo.xml

%changelog
* Sun Oct 11 2026 Alejandro Kurczyn <335237751+a-kurczyn@users.noreply.github.com> - 1.1.0-1
- First package
