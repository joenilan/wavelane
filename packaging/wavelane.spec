# Binary RPM from a staged tree; built by packaging/release.sh.
%global debug_package %{nil}
%global __strip /bin/true

Name:           wavelane
Version:        %{wavelane_version}
Release:        1%{?dist}
Summary:        See where every app's sound goes, and drag it onto any output
License:        GPL-3.0-only
URL:            https://github.com/joenilan/wavelane
Requires:       pipewire >= 1.0
Requires:       wireplumber
# Loaded at runtime by the GPUI window system (not visible to the ELF scan).
Requires:       vulkan-loader
Requires:       libwayland-client
Requires:       libwayland-egl
Recommends:     mesa-vulkan-drivers

%description
Wavelane lives in the tray and shows every application that plays sound on
a map over the screen: drag one onto any output (speakers, headphones, a
GoXLR or Rode channel, a virtual device) and it plays there from now on.
The tray flyout sets each application's volume and output.

%install
cp -a %{wavelane_stage}/. %{buildroot}/

%files
/usr/bin/wavelane
/usr/share/applications/digital.zombie.wavelane.desktop
/usr/share/icons/hicolor/scalable/apps/digital.zombie.wavelane.svg
%license /usr/share/licenses/wavelane/LICENSE
%license /usr/share/licenses/wavelane/THIRD_PARTY_NOTICES.txt
%doc /usr/share/doc/wavelane/README.md
