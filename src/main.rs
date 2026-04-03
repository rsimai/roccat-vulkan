use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use hidapi::{HidApi, HidDevice};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const VID: u16 = 0x1e7d;
const PID: u16 = 0x311a;
const CTRL_INTERFACE: i32 = 1;
const LED_INTERFACE: i32 = 3;
const LED_COUNT: usize = 127;
const STATE_FILE: &str = ".roccat-vulkan-rgb-state.json"; // filename only; resolved relative to $HOME at runtime

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Debug, Serialize, Deserialize)]
struct State {
    leds: Vec<Rgb>,
}

#[derive(Parser, Debug)]
#[command(name = "roccat-vulkan-rgb")]
#[command(about = "Read/write RGB tool for the Roccat Vulkan Pro TKL")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(long)]
    state_file: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Command {
    Get {
        /// Key name (e.g. ESC, A, F5, CAPS_LOCK); see list-keys
        #[arg(long, group = "key_spec")]
        key: Option<String>,
        /// Raw LED matrix index (0..126)
        #[arg(long, group = "key_spec")]
        index: Option<usize>,
    },
    Set {
        /// Key name (e.g. ESC, A, F5, CAPS_LOCK); see list-keys
        #[arg(long, group = "key_spec")]
        key: Option<String>,
        /// Raw LED matrix index (0..126)
        #[arg(long, group = "key_spec")]
        index: Option<usize>,
        #[arg(long)]
        r: u8,
        #[arg(long)]
        g: u8,
        #[arg(long)]
        b: u8,
        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        no_init: bool,
    },
    SetAll {
        #[arg(long)]
        r: u8,
        #[arg(long)]
        g: u8,
        #[arg(long)]
        b: u8,
        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        no_init: bool,
    },
    ListKeys,
    Reset {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        no_init: bool,
    },
}

fn default_state_file() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(STATE_FILE)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let state_file = cli.state_file.unwrap_or_else(default_state_file);
    let mut state = load_state(&state_file)?;

    match cli.command {
        Command::Get { key, index } => {
            let key = resolve_key_spec(key, index)?;

            let c = state.leds[key];
            println!("key={} rgb=({}, {}, {})", key, c.r, c.g, c.b);
            println!("source=tracked-state ({})", state_file.display());
        }
        Command::Set {
            key,
            index,
            r,
            g,
            b,
            dry_run,
            no_init,
        } => {
            let key = resolve_key_spec(key, index)?;

            let old = state.leds[key];
            let new = Rgb { r, g, b };
            state.leds[key] = new;

            println!(
                "key={} old=({}, {}, {}) new=({}, {}, {})",
                key, old.r, old.g, old.b, new.r, new.g, new.b
            );

            if dry_run {
                println!("dry-run: not writing to device");
            } else {
                write_full_frame(&state.leds, !no_init)?;
                println!("device-write=ok");
            }

            save_state(&state_file, &state)?;
        }
        Command::SetAll {
            r,
            g,
            b,
            dry_run,
            no_init,
        } => {
            let new = Rgb { r, g, b };
            state.leds = vec![new; LED_COUNT];

            println!("set-all rgb=({}, {}, {})", r, g, b);

            if dry_run {
                println!("dry-run: not writing to device");
            } else {
                write_full_frame(&state.leds, !no_init)?;
                println!("device-write=ok");
            }

            save_state(&state_file, &state)?;
        }
        Command::ListKeys => {
            for &(name, index) in KEY_ALIASES {
                println!("{:<18} {}", name, index);
            }
        }
        Command::Reset { dry_run, no_init } => {
            state = State {
                leds: vec![Rgb::default(); LED_COUNT],
            };

            if dry_run {
                println!("dry-run: not writing to device");
            } else {
                write_full_frame(&state.leds, !no_init)?;
                println!("device-write=ok");
            }

            save_state(&state_file, &state)?;
            println!("reset to all black");
        }
    }

    Ok(())
}

fn resolve_key_spec(key: Option<String>, index: Option<usize>) -> Result<usize> {
    if let Some(name) = key {
        let normalized = normalize_key_name(&name);
        for &(alias, idx) in KEY_ALIASES {
            if alias == normalized {
                return Ok(idx);
            }
        }
        return Err(anyhow!(
            "unknown key name '{}'; run 'list-keys' to see all valid names",
            name
        ));
    }
    if let Some(idx) = index {
        validate_key(idx)?;
        return Ok(idx);
    }
    Err(anyhow!("specify either --key <name> or --index <number>"))
}

fn validate_key(key: usize) -> Result<()> {
    if key >= LED_COUNT {
        return Err(anyhow!("key index out of range: {} (expected 0..{})", key, LED_COUNT - 1));
    }
    Ok(())
}

fn normalize_key_name(name: &str) -> String {
    name.trim().to_ascii_uppercase().replace('-', "_")
}

const KEY_ALIASES: &[(&str, usize)] = &[
    // ISO Vulkan Pro TKL matrix indices as used by Eruption topology tables.
    ("LEFT_SHIFT", 0),
    ("LEFT_CTRL", 1),
    ("ESC", 2),
    ("GRAVE", 3),
    ("TAB", 4),
    ("CAPS_LOCK", 5),
    ("NONUS_BACKSLASH", 6),
    ("LEFT_WIN", 7),
    ("1", 8),
    ("Q", 9),
    ("A", 10),
    ("Z", 11),
    ("LEFT_ALT", 12),
    ("F1", 13),
    ("2", 14),
    ("W", 15),
    ("S", 16),
    ("X", 17),
    ("3", 21),
    ("E", 22),
    ("D", 23),
    ("C", 24),
    ("4", 26),
    ("R", 27),
    ("F", 28),
    ("V", 29),
    ("F2", 20),
    ("F3", 25),
    ("F4", 30),
    ("5", 31),
    ("T", 32),
    ("G", 33),
    ("B", 34),
    ("SPACE", 35),
    ("6", 36),
    ("Y", 37),
    ("H", 38),
    ("N", 39),
    ("F5", 40),
    ("F6", 47),
    ("7", 41),
    ("U", 42),
    ("J", 43),
    ("M", 44),
    ("8", 48),
    ("I", 49),
    ("K", 50),
    ("COMMA", 51),
    ("F7", 53),
    ("F8", 59),
    ("9", 54),
    ("O", 55),
    ("L", 56),
    ("DOT", 57),
    ("RIGHT_ALT", 58),
    ("F9", 65),
    ("0", 60),
    ("P", 61),
    ("SEMICOLON", 62),
    ("SLASH", 63),
    ("FN", 64),
    ("F10", 71),
    ("MINUS", 66),
    ("LEFT_BRACKET", 67),
    ("APOSTROPHE", 68),
    ("RIGHT_SHIFT", 75),
    ("MENU", 70),
    ("F11", 77),
    ("EQUAL", 72),
    ("RIGHT_BRACKET", 73),
    ("ENTER", 74),
    ("RIGHT_CTRL", 76),
    ("F12", 79),
    ("BACKSPACE", 80),
    ("BACKSLASH", 82),
    ("INSERT", 84),
    ("DELETE", 85),
    ("LEFT", 86),
    ("PRINT", 92),
    ("HOME", 88),
    ("END", 89),
    ("DOWN", 91),
    ("PAGE_UP", 93),
    ("PAGE_DOWN", 94),
    ("UP", 90),
    ("RIGHT", 95),
];

fn open_ctrl_device(api: &HidApi) -> Result<HidDevice> {
    let dev = api
        .device_list()
        .find(|d| d.vendor_id() == VID && d.product_id() == PID && d.interface_number() == CTRL_INTERFACE)
        .ok_or_else(|| {
            anyhow!(
                "could not find Vulkan Pro TKL CTRL interface (vid={:04x} pid={:04x} interface={})",
                VID,
                PID,
                CTRL_INTERFACE
            )
        })?;

    dev.open_device(api)
        .context("failed to open CTRL HID device (permissions?)")
}

fn send_init_sequence(ctrl: &HidDevice) -> Result<()> {
    const WAIT_MS: u64 = 10;

    let report_0d: [u8; 16] = [
        0x0d, 0x10, 0x00, 0x00, 0x02, 0x0f, 0x45, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];
    let report_0e: [u8; 5] = [0x0e, 0x05, 0x01, 0x00, 0x00];
    let report_11: [u8; 299] = [
        0x11, 0x2b, 0x01, 0x00, 0x09, 0x06, 0x45, 0x80, 0x00, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x0a, 0x0a, 0x0a,
        0x0a, 0x0a, 0x0a, 0x11, 0x11, 0x11, 0x11, 0x17, 0x17, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x17, 0x17, 0x17,
        0x17, 0x1e, 0x1e, 0x1e, 0x1e, 0x1e, 0x1e, 0x1e, 0x25, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x25, 0x25, 0x25,
        0x25, 0x2b, 0x2b, 0x2b, 0x2b, 0x32, 0x32, 0x39, 0x39, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x32, 0x39, 0x39,
        0x3f, 0x39, 0x39, 0x3f, 0x3f, 0x46, 0x46, 0x46, 0x3f, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xfe, 0xff, 0x3f, 0x46, 0x46,
        0x4d, 0x4d, 0x46, 0x46, 0x4d, 0x4d, 0x53, 0x53, 0x4d, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfe, 0xfe, 0xfc,
        0xfc, 0xfc, 0xfc, 0xfc, 0xfc, 0xfa, 0xfa, 0xfa, 0xfa, 0x53, 0x53, 0x57,
        0x57, 0x57, 0x57, 0x57, 0x57, 0x5c, 0x5c, 0x5c, 0x5c, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfa, 0xfa, 0xf8,
        0xf6, 0xf6, 0xf8, 0xf8, 0xf6, 0xf6, 0xf6, 0xf6, 0x00, 0x5c, 0x5c, 0x62,
        0x66, 0x66, 0x62, 0x62, 0x66, 0x66, 0x66, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf4, 0xf4, 0xf4,
        0x00, 0xf1, 0xf1, 0xf1, 0xf1, 0xf4, 0xef, 0xef, 0xef, 0x6b, 0x6b, 0x6b,
        0x00, 0x71, 0x71, 0x71, 0x71, 0x6b, 0x75, 0x75, 0x75, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4a, 0x75,
    ];

    ctrl.send_feature_report(&report_0d)
        .context("failed to send init report 0x0d")?;
    thread::sleep(Duration::from_millis(WAIT_MS));
    ctrl.send_feature_report(&report_0e)
        .context("failed to send init report 0x0e")?;
    thread::sleep(Duration::from_millis(WAIT_MS));
    ctrl.send_feature_report(&report_11)
        .context("failed to send init report 0x11")?;
    thread::sleep(Duration::from_millis(WAIT_MS));

    Ok(())
}

fn open_led_device(api: &HidApi) -> Result<HidDevice> {
    let dev = api
        .device_list()
        .find(|d| d.vendor_id() == VID && d.product_id() == PID && d.interface_number() == LED_INTERFACE)
        .ok_or_else(|| {
            anyhow!(
                "could not find Vulkan Pro TKL LED interface (vid={:04x} pid={:04x} interface={})",
                VID,
                PID,
                LED_INTERFACE
            )
        })?;

    dev.open_device(api)
        .context("failed to open LED HID device (permissions?)")
}

fn write_full_frame(led_map: &[Rgb], initialize: bool) -> Result<()> {
    let api = HidApi::new().context("failed to initialize hidapi")?;
    let led = open_led_device(&api)?;

    if initialize {
        let ctrl = open_ctrl_device(&api)?;
        // Switch keyboard into host-driven LED mode before pushing a frame.
        send_init_sequence(&ctrl)?;
    }

    write_led_map(&led, led_map)
}

fn write_led_map(device: &HidDevice, led_map: &[Rgb]) -> Result<()> {
    if led_map.len() < LED_COUNT {
        return Err(anyhow!("short LED map: got {}, expected {}", led_map.len(), LED_COUNT));
    }

    let mut frame = [0u8; 448];

    // Protocol layout used by the Roccat Vulkan Pro TKL family.
    for (i, color) in led_map.iter().take(LED_COUNT).enumerate() {
        let offset = ((i / 12) * 36) + (i % 12);
        frame[offset] = color.r;
        frame[offset + 12] = color.g;
        frame[offset + 24] = color.b;
    }

    for (chunk_index, chunk) in frame.chunks(60).take(5).enumerate() {
        let mut packet = [0u8; 64];
        if chunk_index == 0 {
            packet[0..4].copy_from_slice(&[0xa1, 0x01, 0x34, 0x01]);
        } else {
            packet[0..4].copy_from_slice(&[0xa1, (chunk_index as u8) + 1, 0x00, 0x00]);
        }
        packet[4..64].copy_from_slice(chunk);

        let written = device
            .write(&packet)
            .with_context(|| format!("failed writing packet {}", chunk_index + 1))?;

        if written != 64 {
            return Err(anyhow!(
                "short HID write on packet {}: {} bytes",
                chunk_index + 1,
                written
            ));
        }
    }

    Ok(())
}

fn load_state(path: &Path) -> Result<State> {
    if !path.exists() {
        return Ok(State {
            leds: vec![Rgb::default(); LED_COUNT],
        });
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read state file {}", path.display()))?;
    let mut state: State = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse state file {}", path.display()))?;

    if state.leds.len() != LED_COUNT {
        state.leds.resize(LED_COUNT, Rgb::default());
    }

    Ok(state)
}

fn save_state(path: &Path, state: &State) -> Result<()> {
    let data = serde_json::to_string_pretty(state).context("failed to serialize state")?;
    fs::write(path, data).with_context(|| format!("failed to write state file {}", path.display()))
}
