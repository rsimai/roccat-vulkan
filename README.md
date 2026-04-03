# roccat-vulkan-rgb

A small tool for writing RGB values to a ROCCAT Vulkan Pro TKL keyboard and reading back the values tracked by this tool. Works on openSUSE, I didn't try with any other keyboard models, see the udev rule.

## What "read" means here

The keyboard LED protocol used here is write-focused, I couldn't figure out how to read from the device. `get` reads the value from this tool's tracked state file (`.roccat-vulkan-rgb-state.json`) in user's $HOME directory, if not specified otherwise.

That means:
- `set` updates the key(s) in the state file and writes a (full) frame to the device
- `get` returns the tracked value for the key(s) from the state file, not from the device

If another process or onboard effect changes lighting, `get` will not reflect those external changes.

## Setup (one-time, no root required after this)

Install the bundled udev rule so Linux grants the physically-logged-in user access to the keyboard's HID device nodes:

```bash
sudo cp 99-roccat-vulkan-pro-tkl.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --action=add /dev/hidraw*
```

No group membership changes are needed. The rule uses the systemd `uaccess` tag, which dynamically grants access only to the user at the local physical seat (seat0). Users connected via SSH or other non-seat sessions are not granted access. The ACL is removed automatically when the local user logs out.

## Build

Quick run with
```bash
cargo run -- set-all --r 200 --g 255 --b 00
```
or build the binary with

```bash
cargo build --release
```
and copy the resulting target/release/roccat-vulkan-rgb to your ~/bin or whatever your preference is.

## Usage

Read key color from tracked state:

```bash
roccat-vulkan-rgb get --key 10
# or by key name
roccat-vulkan-rgb get --key F5
```

Set key color and write to keyboard:

```bash
roccat-vulkan-rgb set --key 10 --r 255 --g 40 --b 0
# or by key name
roccat-vulkan-rgb set --key CAPS_LOCK --r 0 --g 180 --b 255
```

Set all keys in one write (fast path):

```bash
roccat-vulkan-rgb set-all --r 255 --g 0 --b 0
```

Skip control init for repeated writes (faster, use only when keyboard is already in host mode):

```bash
roccat-vulkan-rgb set-all --r 0 --g 0 --b 255 --no-init
```

Dry run without writing to keyboard:

```bash
roccat-vulkan-rgb set --key 10 --r 255 --g 40 --b 0 --dry-run
```

Reset tracked state to black:

```bash
roccat-vulkan-rgb reset
```

List available key names:

```bash
roccat-vulkan-rgb list-keys
```

## Notes

- Key accepts a raw LED index (0..126) or a key name (for example `ESC`, `A`, `F5`, `CAPS_LOCK`).
- Named keys are mapped to the Vulkan Pro TKL ISO matrix indices used by Eruption device tables.
- The tool communicates with the keyboard over two HID interfaces (USB `VID 1e7d` / `PID 311a`): the LED interface for color frames and the control interface for the host-mode init sequence.
- On `set`, the control-interface init sequence is sent first unless `--no-init` is given.
- `set-all` is much faster than calling `set` many times because it updates all keys with one frame write.
- For speed-sensitive loops, use the compiled binary directly instead of `cargo run`.
