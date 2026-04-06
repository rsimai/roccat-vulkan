use eframe::egui;
use evdev::{Device, EventSummary, EventType};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::Duration;

/// Map evdev `KeyCode` debug names (e.g. `"KEY_LEFTSHIFT"`) to the KEY_ALIASES
/// names used in templates (e.g. `"LEFT_SHIFT"`).  Only keys that appear in
/// KEY_ALIASES are included; anything unmapped is kept as-is.
fn evdev_to_alias(evdev_name: &str) -> String {
    // Strip the leading "KEY_" prefix the evdev Debug impl emits, then match.
    let bare = evdev_name.strip_prefix("KEY_").unwrap_or(evdev_name);
    // Non-obvious renames; most single-char / Fx keys match after prefix strip.
    let mapped = match bare {
        "LEFTSHIFT"      => "LEFT_SHIFT",
        "LEFTCTRL"       => "LEFT_CTRL",
        "LEFTALT"        => "LEFT_ALT",
        "LEFTMETA"       => "LEFT_WIN",
        "RIGHTSHIFT"     => "RIGHT_SHIFT",
        "RIGHTCTRL"      => "RIGHT_CTRL",
        "RIGHTALT"       => "RIGHT_ALT",
        "CAPSLOCK"       => "CAPS_LOCK",
        "102ND"          => "NONUS_BACKSLASH",
        "COMPOSE"        => "MENU",
        "SYSRQ"          => "PRINT",
        "LEFTBRACE"      => "LEFT_BRACKET",
        "RIGHTBRACE"     => "RIGHT_BRACKET",
        "PAGEUP"         => "PAGE_UP",
        "PAGEDOWN"       => "PAGE_DOWN",
        "DOT"            => "DOT",
        "COMMA"          => "COMMA",
        "SEMICOLON"      => "SEMICOLON",
        "APOSTROPHE"     => "APOSTROPHE",
        "GRAVE"          => "GRAVE",
        "MINUS"          => "MINUS",
        "EQUAL"          => "EQUAL",
        "BACKSLASH"      => "BACKSLASH",
        "SLASH"          => "SLASH",
        "SPACE"          => "SPACE",
        "TAB"            => "TAB",
        "ENTER"          => "ENTER",
        "ESC"            => "ESC",
        "BACKSPACE"      => "BACKSPACE",
        "INSERT"         => "INSERT",
        "DELETE"         => "DELETE",
        "HOME"           => "HOME",
        "END"            => "END",
        "UP"             => "UP",
        "DOWN"           => "DOWN",
        "LEFT"           => "LEFT",
        "RIGHT"          => "RIGHT",
        "FN"             => "FN",
        other            => other, // single chars (A–Z, 0–9, F1–F12) match directly
    };
    mapped.to_string()
}

/// Pick the best keyboard device from an iterator of (path, device) pairs.
///
/// A device is a keyboard candidate when it has ALL of:
///   - `EV_KEY` (key events)
///   - `EV_LED` (Caps Lock / Num Lock LEDs) — distinguishes full keyboards from
///     consumer-control / media-key-only HID interfaces and from mice.
///   - NOT `EV_REL` (relative axes — that's a mouse or trackpad).
///
/// Among candidates the Roccat VID (0x1e7d) is preferred; ties broken by
/// highest `EV_KEY` count.
fn pick_keyboard(devices: impl IntoIterator<Item = (std::path::PathBuf, Device)>) -> Option<Device> {
    const ROCCAT_VID: u16 = 0x1e7d;

    let mut best_count = 0usize;
    let mut best_is_roccat = false;
    let mut best: Option<Device> = None;

    for (_, dev) in devices {
        let events = dev.supported_events();
        // Must have key events.
        if !events.contains(EventType::KEY) {
            continue;
        }
        // Exclude mice / trackpads (they use relative axes).
        if events.contains(EventType::RELATIVE) {
            continue;
        }
        // Require LED events (Caps Lock etc.). This rules out consumer-control
        // HID interfaces that expose only media keys with no indicator LEDs.
        if !events.contains(EventType::LED) {
            continue;
        }
        let key_count = dev.supported_keys().map_or(0, |k| k.iter().count());
        if key_count == 0 {
            continue;
        }
        let is_roccat = dev.input_id().vendor() == ROCCAT_VID;
        // Pick this device if it has a better "score": Roccat VID beats generic;
        // among same VID class, higher key count wins.
        let better = match best {
            None => true,
            _ => (is_roccat && !best_is_roccat)
                || (is_roccat == best_is_roccat && key_count > best_count),
        };
        if better {
            best_count = key_count;
            best_is_roccat = is_roccat;
            best = Some(dev);
        }
    }

    best
}

/// Run as a privileged helper subprocess (invoked via `pkexec self --evdev-helper`).
///
/// Enumerates `/dev/input/event*` (as root), picks the keyboard, grabs it
/// exclusively, then writes to stdout:
///   - `"OK\n"` on success, followed by `"KEY_FOO\n"` lines for every key press, or
///   - `"ERR:<message>\n"` and exits on failure.
///
/// Exits cleanly (releasing the grab) when the parent drops the stdin write end.
pub fn evdev_helper() {
    use std::io::{BufWriter, Read, Write};

    let devices: Vec<_> = evdev::enumerate().collect();

    let mut dev = match pick_keyboard(devices) {
        Some(d) => d,
        None => {
            println!("ERR:No keyboard device found under /dev/input/");
            return;
        }
    };

    if let Err(e) = dev.grab() {
        println!("ERR:grab failed: {e}");
        return;
    }

    if let Err(e) = dev.set_nonblocking(true) {
        let _ = dev.ungrab();
        println!("ERR:set_nonblocking: {e}");
        return;
    }

    // Watch stdin: when the parent drops the ChildStdin write end (stop_learn),
    // read() returns 0 (EOF).  Set the stop flag so the main loop exits cleanly
    // and calls dev.ungrab() before this process exits.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_stdin = Arc::clone(&stop);
    thread::spawn(move || {
        let mut buf = [0u8; 1];
        loop {
            match io::stdin().read(&mut buf) {
                Ok(0) | Err(_) => {
                    stop_stdin.store(true, Ordering::Relaxed);
                    return;
                }
                Ok(_) => {}
            }
        }
    });

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    macro_rules! write_line {
        ($($arg:tt)*) => {
            if writeln!(out, $($arg)*).is_err() || out.flush().is_err() {
                // stdout closed; parent went away — exit and release grab via drop
                stop.store(true, Ordering::Relaxed);
            }
        };
    }

    write_line!("OK");

    while !stop.load(Ordering::Relaxed) {
        match dev.fetch_events() {
            Ok(events) => {
                for ev in events {
                    if let EventSummary::Key(_, keycode, 1) = ev.destructure() {
                        write_line!("{keycode:?}");
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                write_line!("ERR:{e}");
                break;
            }
        }
    }

    let _ = dev.ungrab();
}

pub fn run(state_file: &std::path::Path) -> eframe::Result<()> {
    // Load the current LED state so we can use it as the base for live preview.
    let base_leds = crate::load_state(state_file)
        .map(|s| s.leds)
        .unwrap_or_else(|_| vec![crate::Rgb::default(); crate::LED_COUNT]);

    // Always default to the standard state file path; the user can edit it in the UI.
    let save_path = state_file.to_string_lossy().into_owned();

    // Pre-populate key_colors and set_all_color from whichever file we'll save to.
    let (key_colors, set_all_color) = load_template(&save_path);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 520.0])
            .with_title("Roccat Vulkan RGB — Key Editor"),
        ..Default::default()
    };
    eframe::run_native(
        "Roccat Vulkan RGB — Key Editor",
        options,
        Box::new(move |_cc| Ok(Box::new(EditorApp::new(base_leds, key_colors, set_all_color, save_path)))),
    )
}

struct EditorApp {
    learn_mode: bool,
    /// Keys toggled during the current learn session (cleared when learn starts).
    selected_keys: BTreeSet<String>,
    /// Accumulated key → color assignments across multiple learn/color rounds.
    key_colors: BTreeMap<String, egui::Color32>,
    /// Background color applied to all LEDs via [set-all] (None = no set-all section).
    set_all_color: Option<egui::Color32>,
    /// The color currently chosen in the color picker.
    chosen_color: egui::Color32,
    /// Whether the preview LED frame needs to be re-sent to the keyboard.
    preview_dirty: bool,
    /// Base LED state loaded at startup; used as the background for live preview.
    base_leds: Vec<crate::Rgb>,
    /// Whether the first live-preview HID write (with init) has been done.
    preview_init_done: bool,
    /// Save-path text field.
    save_path: String,
    /// Feedback after a save attempt.
    save_msg: Option<String>,
    /// Receives key-name strings (or "__ERR__:<msg>") from the evdev thread.
    key_rx: Option<mpsc::Receiver<String>>,
    /// Set to `true` to ask the direct-evdev thread to stop polling.
    stop_flag: Option<Arc<AtomicBool>>,
    /// Shared handle to a pkexec child process (for reaping).
    child_slot: Arc<Mutex<Option<std::process::Child>>>,
    /// Write end of the stdin pipe to the pkexec helper.
    helper_stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    learn_thread: Option<thread::JoinHandle<()>>,
    /// Non-fatal status / error message shown in the UI.
    status_msg: Option<String>,
}

impl EditorApp {
    fn new(base_leds: Vec<crate::Rgb>, key_colors: BTreeMap<String, egui::Color32>, set_all_color: Option<egui::Color32>, save_path: String) -> Self {
        Self {
            learn_mode: false,
            selected_keys: BTreeSet::new(),
            key_colors,
            set_all_color,
            chosen_color: egui::Color32::from_rgb(255, 255, 255),
            preview_dirty: false,
            base_leds,
            preview_init_done: false,
            save_path,
            save_msg: None,
            key_rx: None,
            stop_flag: None,
            child_slot: Arc::new(Mutex::new(None)),
            helper_stdin: Arc::new(Mutex::new(None)),
            learn_thread: None,
            status_msg: None,
        }
    }

    /// Build the preview LED frame: set_all → key_colors applied on top.
    fn preview_leds(&self) -> Vec<crate::Rgb> {
        let mut leds = self.base_leds.clone();
        // Apply set-all background first.
        if let Some(c) = self.set_all_color {
            let rgb = crate::Rgb { r: c.r(), g: c.g(), b: c.b() };
            for led in &mut leds {
                *led = rgb;
            }
        }
        for (key, &color) in &self.key_colors {
            if let Some(&(_, idx)) = crate::KEY_ALIASES.iter().find(|&&(name, _)| name == key.as_str()) {
                if idx < leds.len() {
                    leds[idx] = crate::Rgb { r: color.r(), g: color.g(), b: color.b() };
                }
            }
        }
        leds
    }

    /// Commit selected_keys into key_colors with the current chosen_color.
    fn commit_selection(&mut self) {
        for key in &self.selected_keys {
            self.key_colors.insert(key.clone(), self.chosen_color);
        }
        self.preview_dirty = true;
    }

    /// Spawn a background thread to push a preview frame to the keyboard.
    fn send_preview(&mut self) {
        let leds = self.preview_leds();
        let init = !self.preview_init_done;
        self.preview_init_done = true;
        self.preview_dirty = false;
        thread::spawn(move || {
            let _ = crate::write_full_frame(&leds, init);
        });
    }
}

/// Parse the template TOML at `path`.
/// Returns (key_colors, set_all_color); empty / None if file missing or unparseable.
fn load_template(path: &str) -> (BTreeMap<String, egui::Color32>, Option<egui::Color32>) {
    #[derive(serde::Deserialize)]
    struct SetAllDoc {
        #[serde(rename = "ALL")]
        all: Option<Vec<u8>>,
    }
    #[derive(serde::Deserialize)]
    struct TemplateDoc {
        #[serde(rename = "set-all")]
        set_all: Option<SetAllDoc>,
        key: Option<BTreeMap<String, Vec<u8>>>,
    }

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (BTreeMap::new(), None),
    };
    let doc: TemplateDoc = match toml::from_str(&text) {
        Ok(d) => d,
        Err(_) => return (BTreeMap::new(), None),
    };
    let set_all_color = doc.set_all
        .and_then(|sa| sa.all)
        .filter(|rgb| rgb.len() >= 3)
        .map(|rgb| egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    let mut key_colors = BTreeMap::new();
    if let Some(keys) = doc.key {
        for (name, rgb) in keys {
            if rgb.len() >= 3 {
                key_colors.insert(name, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            }
        }
    }
    (key_colors, set_all_color)
}

impl EditorApp {
    fn start_learn(&mut self) {
        // Clear the working selection for a fresh learn session.
        // key_colors (previous assignments) are preserved.
        self.selected_keys.clear();
        let stop = Arc::new(AtomicBool::new(false));
        let child_slot = Arc::new(Mutex::new(None::<std::process::Child>));
        let helper_stdin = Arc::new(Mutex::new(None::<std::process::ChildStdin>));
        self.child_slot = Arc::clone(&child_slot);
        self.helper_stdin = Arc::clone(&helper_stdin);
        let (tx, rx) = mpsc::sync_channel(256);
        let handle = spawn_evdev_thread(tx, Arc::clone(&stop), child_slot, helper_stdin);
        self.stop_flag = Some(stop);
        self.key_rx = Some(rx);
        self.learn_thread = Some(handle);
        self.learn_mode = true;
        self.status_msg = None;
    }

    fn stop_learn(&mut self) {
        // Tell the direct-evdev thread to stop polling.
        if let Some(flag) = self.stop_flag.take() {
            flag.store(true, Ordering::Relaxed);
        }
        // Signal the pkexec helper to exit cleanly by closing its stdin pipe.
        if let Ok(mut guard) = self.helper_stdin.lock() {
            *guard = None; // drop ChildStdin → EOF on helper's stdin
        }
        // Move join() off the UI thread so the button stays responsive.
        if let Some(handle) = self.learn_thread.take() {
            thread::spawn(move || {
                let _ = handle.join();
            });
        }
        self.key_rx = None;
        self.learn_mode = false;
        // Commit selected_keys into key_colors and push a preview frame.
        self.commit_selection();
        self.send_preview();
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain key events produced by the evdev background thread.
        let mut had_error = false;
        if let Some(rx) = &self.key_rx {
            while let Ok(name) = rx.try_recv() {
                if let Some(msg) = name.strip_prefix("__ERR__:") {
                    self.status_msg = Some(format!("Error: {msg}"));
                    had_error = true;
                    break;
                }
                // Map evdev key name to KEY_ALIASES name, then toggle.
                let alias = evdev_to_alias(&name);
                if !self.selected_keys.remove(&alias) {
                    self.selected_keys.insert(alias);
                }
            }
        }
        if had_error {
            self.stop_learn();
        }

        // While learning, keep the UI polling so key events are processed promptly.
        if self.learn_mode {
            ctx.request_repaint_after(Duration::from_millis(16));
        }

        // Live preview: re-send when key_colors changed or chosen_color changed
        // (which updates selected_keys entries in key_colors).
        if !self.learn_mode && self.preview_dirty {
            self.send_preview();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Roccat Vulkan RGB — Key Editor");

            // Inform the user when the save file doesn't exist yet.
            if !std::path::Path::new(&self.save_path).exists() {
                ui.colored_label(
                    egui::Color32::from_rgb(180, 140, 0),
                    "⚠ No existing file found — starting from scratch.",
                );
            }

            ui.separator();

            // ── Learn mode controls ─────────────────────────────────────────
            let btn_label = if self.learn_mode {
                "⏹  Stop Learning"
            } else {
                "▶  Learn"
            };

            if ui.button(btn_label).clicked() {
                if self.learn_mode {
                    self.stop_learn();
                } else {
                    self.start_learn();
                }
            }

            if self.learn_mode {
                ui.add_space(4.0);
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 0),
                    "Learning active — press keys to toggle selection",
                );
            }

            if let Some(msg) = &self.status_msg {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(255, 80, 80), msg);
            }

            ui.separator();

            // ── All assigned key→color pairs — always visible ───────────────
            ui.label(format!(
                "Assigned: {}   |   Selecting: {}",
                self.key_colors.len(),
                self.selected_keys.len()
            ));
            ui.add_space(4.0);

            // During learn: show the current working selection (no swatches yet).
            // After learn: show all key_colors with per-key color swatches.
            let is_learning = self.learn_mode;
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    if is_learning {
                        // Show current in-progress selection.
                        let keys: Vec<_> = self.selected_keys.iter().cloned().collect();
                        for key in keys {
                            ui.horizontal(|ui| {
                                ui.label(&key);
                                if ui.small_button("✕").clicked() {
                                    self.selected_keys.remove(&key);
                                }
                            });
                        }
                    } else {
                        // Show all committed assignments with their individual colors.
                        let entries: Vec<_> = self.key_colors.iter()
                            .map(|(k, &c)| (k.clone(), c))
                            .collect();
                        for (key, color) in entries {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(18.0, 14.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().rect_filled(rect, 2.0, color);
                                // Highlight if in current selection.
                                if self.selected_keys.contains(&key) {
                                    ui.strong(&key);
                                } else {
                                    ui.label(&key);
                                }
                                if ui.small_button("✕").clicked() {
                                    self.key_colors.remove(&key);
                                    self.selected_keys.remove(&key);
                                    self.preview_dirty = true;
                                }
                            });
                        }
                    }
                });

            // ── Color chooser + save (only shown after leaving learn mode) ──
            if !self.learn_mode {
                ui.separator();

                // ── Background color (set-all) ──────────────────────────
                ui.horizontal(|ui| {
                    let mut enabled = self.set_all_color.is_some();
                    if ui.checkbox(&mut enabled, "Background (set-all)").changed() {
                        self.set_all_color = if enabled {
                            Some(egui::Color32::from_rgb(0, 0, 0))
                        } else {
                            None
                        };
                        self.preview_dirty = true;
                    }
                    if let Some(ref mut c) = self.set_all_color {
                        let before = *c;
                        ui.color_edit_button_srgba(c);
                        if *c != before {
                            self.preview_dirty = true;
                        }
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Color for selected keys:");
                    let before = self.chosen_color;
                    ui.color_edit_button_srgba(&mut self.chosen_color);
                    // When color changes, update all selected_keys in key_colors live.
                    if self.chosen_color != before && !self.selected_keys.is_empty() {
                        let color = self.chosen_color;
                        for key in &self.selected_keys {
                            self.key_colors.insert(key.clone(), color);
                        }
                        self.preview_dirty = true;
                    }
                    if ui.button("Clear all").clicked() {
                        self.key_colors.clear();
                        self.selected_keys.clear();
                        self.save_msg = None;
                        self.preview_dirty = true;
                    }
                });

                ui.separator();

                // ── Save template ──────────────────────────────────────────
                ui.label("Save as template:");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_path)
                            .desired_width(300.0)
                            .hint_text("path/to/template.toml"),
                    );
                    let can_save = (!self.key_colors.is_empty() || self.set_all_color.is_some()) && !self.save_path.is_empty();
                    if ui
                        .add_enabled(can_save, egui::Button::new("💾  Write Template"))
                        .clicked()
                    {
                        let result = check_is_template_or_new(&self.save_path)
                            .and_then(|()| write_template(&self.save_path, self.set_all_color, &self.key_colors));
                        match result {
                            Ok(()) => {
                                self.save_msg = Some(format!("Saved → {}", self.save_path));
                            }
                            Err(e) => {
                                self.save_msg = Some(format!("Error: {e}"));
                            }
                        }
                    }
                });

                if let Some(msg) = &self.save_msg {
                    let color = if msg.starts_with("Error") {
                        egui::Color32::from_rgb(255, 80, 80)
                    } else {
                        egui::Color32::from_rgb(80, 220, 80)
                    };
                    ui.colored_label(color, msg);
                }
            }
        });
    }
}

/// Returns Ok(()) if the path either does not exist yet, or exists and parses
/// as a valid roccat-vulkan-rgb template (has at least one of [set-all], [key],
/// or [index]).  Returns an error if the file exists but is not a template, to
/// prevent silently overwriting unrelated files.
fn check_is_template_or_new(path: &str) -> std::io::Result<()> {
    #[derive(serde::Deserialize)]
    struct AnyTemplate {
        #[serde(rename = "set-all")]
        set_all: Option<toml::Value>,
        key: Option<toml::Value>,
        index: Option<toml::Value>,
    }
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(p)?;
    let doc: Result<AnyTemplate, _> = toml::from_str(&text);
    match doc {
        Ok(t) if t.set_all.is_some() || t.key.is_some() || t.index.is_some() => Ok(()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exists but contains no [set-all], [key], or [index] section — refusing to overwrite",
        )),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exists but is not a valid TOML template — refusing to overwrite",
        )),
    }
}

/// Write a TOML template file from accumulated key→color assignments.
fn write_template(
    path: &str,
    set_all_color: Option<egui::Color32>,
    key_colors: &BTreeMap<String, egui::Color32>,
) -> std::io::Result<()> {
    use std::io::Write;
    let parent = std::path::Path::new(path).parent();
    if let Some(p) = parent {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p)?;
        }
    }
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "# Roccat Vulkan Pro TKL — RGB template")?;
    writeln!(f, "# Generated by roccat-vulkan-rgb editor")?;
    writeln!(f, "# Colors are [R, G, B] values in range 0–255.")?;
    writeln!(f, "# Applied in order: [set-all] → [key] → [index]")?;
    writeln!(f)?;
    if let Some(c) = set_all_color {
        writeln!(f, "[set-all]")?;
        writeln!(f, "ALL               = [{:>3}, {:>3}, {:>3}]", c.r(), c.g(), c.b())?;
        writeln!(f)?;
    }
    if !key_colors.is_empty() {
        writeln!(f, "[key]")?;
        for (key, &color) in key_colors {
            writeln!(f, "{:<18} = [{:>3}, {:>3}, {:>3}]", key, color.r(), color.g(), color.b())?;
        }
    }
    Ok(())
}

/// Spawns the evdev interception thread.
///
/// Strategy:
///   1. Try to enumerate `/dev/input/event*` directly.  This succeeds when the
///      user is in the `input` group or is root — the happy path.
///   2. If enumeration returns nothing (i.e. permission denied for all devices),
///      fall back to launching `pkexec <self> --evdev-helper` as root.  The
///      helper opens and grabs the device and streams key-name lines to stdout;
///      this process reads them.  Killing the child (in `stop_learn`) causes
///      the blocking `read_line` to return EOF, cleanly ending the thread.
fn spawn_evdev_thread(
    tx: mpsc::SyncSender<String>,
    stop: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<std::process::Child>>>,
    helper_stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let devices: Vec<_> = evdev::enumerate().collect();
        if !devices.is_empty() {
            run_direct_evdev(devices, &tx, &stop);
        } else {
            run_via_pkexec(&tx, &child_slot, &helper_stdin);
        }
    })
}

/// Direct path: we already have read access to the device files.
fn run_direct_evdev(
    devices: Vec<(std::path::PathBuf, evdev::Device)>,
    tx: &mpsc::SyncSender<String>,
    stop: &AtomicBool,
) {
    let mut dev = match pick_keyboard(devices) {
        Some(d) => d,
        None => {
            let _ = tx.send("__ERR__:No keyboard device found under /dev/input/".into());
            return;
        }
    };

    if let Err(e) = dev.set_nonblocking(true) {
        let _ = tx.send(format!("__ERR__:set_nonblocking: {e}"));
        return;
    }

    if let Err(e) = dev.grab() {
        let _ = tx.send(format!("__ERR__:grab failed ({e})"));
        return;
    }

    while !stop.load(Ordering::Relaxed) {
        match dev.fetch_events() {
            Ok(events) => {
                for ev in events {
                    if let EventSummary::Key(_, keycode, 1) = ev.destructure() {
                        if tx.send(format!("{keycode:?}")).is_err() {
                            return; // device drop releases the grab
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = tx.send(format!("__ERR__:fetch_events: {e}"));
                break;
            }
        }
    }

    let _ = dev.ungrab();
}

/// Elevated path: spawn `pkexec <self> --evdev-helper` and read key-name lines
/// from the child's stdout pipe.  The child grabs the device as root.
/// `stop_learn()` kills the child which causes `read_line` here to return EOF.
fn run_via_pkexec(
    tx: &mpsc::SyncSender<String>,
    child_slot: &Mutex<Option<std::process::Child>>,
    helper_stdin: &Mutex<Option<std::process::ChildStdin>>,
) {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(format!("__ERR__:current_exe: {e}"));
            return;
        }
    };

    let mut child = match Command::new("pkexec")
        .arg(&exe)
        .arg("--evdev-helper")
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())  // write end kept alive in helper_stdin; dropping it sends EOF
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(format!("__ERR__:pkexec launch failed: {e}"));
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    // Store the stdin write end so stop_learn() can close it (EOF → helper exits).
    *helper_stdin.lock().unwrap() = child.stdin.take();
    // Store child for reaping.
    *child_slot.lock().unwrap() = Some(child);

    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    // First line is "OK" or "ERR:<message>".
    match reader.read_line(&mut line) {
        Ok(0) => {
            let _ = tx.send("__ERR__:Helper exited without response (authentication cancelled?)".into());
            return;
        }
        Ok(_) => {
            let trimmed = line.trim_end();
            if let Some(msg) = trimmed.strip_prefix("ERR:") {
                let _ = tx.send(format!("__ERR__:{msg}"));
                return;
            }
            if trimmed != "OK" {
                let _ = tx.send(format!("__ERR__:Unexpected helper response: {trimmed}"));
                return;
            }
        }
        Err(e) => {
            let _ = tx.send(format!("__ERR__:Reading helper stdout: {e}"));
            return;
        }
    }

    // Subsequent lines are key names or "ERR:…".
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF — child was killed (stop_learn) or crashed
            Ok(_) => {
                let name = line.trim_end();
                if let Some(msg) = name.strip_prefix("ERR:") {
                    let _ = tx.send(format!("__ERR__:{msg}"));
                    break;
                }
                if tx.send(name.to_string()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // Reap the zombie (child may already be dead from stop_learn's kill()).
    if let Ok(mut guard) = child_slot.lock() {
        if let Some(c) = guard.as_mut() {
            let _ = c.wait();
        }
        *guard = None;
    }
}
