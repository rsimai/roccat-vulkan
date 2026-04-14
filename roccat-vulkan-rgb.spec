Name:           roccat-vulkan-rgb
Version:        0.4.0
Release:        0
Summary:        RGB control tool for the ROCCAT Vulkan Pro TKL keyboard
License:        GPL-3.0-or-later
URL:            https://github.com/rsimai/roccat-vulkan
Source0:        %{name}-%{version}.tar.gz
Source1:        vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  pkgconfig(libudev)

%description
roccat-vulkan-rgb is a small command-line tool for writing RGB values to a
ROCCAT Vulkan Pro TKL keyboard.  It tracks the LED state in a per-user state
file and writes full frames directly to the keyboard's HID interfaces.

Features:
  - Set individual key colors by name or index
  - Set all keys in a single frame write
  - Save and load TOML lighting templates (human-readable, hand-editable)
  - Apply saved state to the device (useful at login or after USB reconnect)
  - Dry-run mode for testing without touching the hardware
  - udev rule for access without root

%prep
%autosetup -p1
tar -xf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml << 'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
cargo build --release

%install
install -Dm 0755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dm 0644 99-roccat-vulkan-pro-tkl.rules \
    %{buildroot}%{_prefix}/lib/udev/rules.d/99-roccat-vulkan-pro-tkl.rules

%post
udevadm control --reload-rules || :

%files
%license LICENSE
%doc README.md example-template.toml 50-roccat-vulkan-rgb.rules
%{_bindir}/%{name}
/usr/lib/udev/rules.d/99-roccat-vulkan-pro-tkl.rules

%changelog
* Mon Apr 14 2026 Robert Simai <robert@simai.net> - 0.4.0
- Add effect subcommand with --intensity option to scale LED brightness
  without modifying the saved state file
- Add --intensity option to load-template; template colours are saved
  unscaled, only the frame sent to the device is scaled
