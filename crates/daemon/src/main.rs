//! BigSound daemon — exposes the live filter-chain controls of BigSound
//! over D-Bus (for GUI / CLI clients) and auto-applies per-device tuning
//! profiles when the system default sink changes.
//!
//! Architecture (v0.5):
//!
//!   client (GUI/CLI)  ───── D-Bus ────►  daemon  ── pw-cli ─►  PipeWire
//!                                          │
//!                                          ├── in-memory cache (source of truth)
//!                                          ├── profiles dir (JSON files)
//!                                          └── background thread
//!                                                ├── re-pushes cache to PipeWire
//!                                                │   every 3s (handles suspend→resume)
//!                                                └── polls default sink, auto-applies
//!                                                    matching profile when it changes
//!
//! D-Bus interface (com.bigcommunity.BigSound1):
//!   Set(name, value)             → cache + push to PipeWire
//!   Get(name) → value            → cache read
//!   List() → Vec<name>           → all parameters
//!   ListProfiles() → Vec<name>   → all profile names
//!   GetProfile(name) → JSON      → describe a profile
//!   ApplyProfile(name)           → apply a profile by name
//!   SaveProfile(name)            → snapshot current cache as a named profile
//!   DeleteProfile(name)          → remove a profile from disk
//!   ListOutputDevices() → Vec<(name, description)>  → real sinks to route to
//!   GetOutputDevice() → name     → chosen output sink ("" = automatic)
//!   SetOutputDevice(name)        → pin BigSound.output to a sink ("" = auto)
//!   property NodeId, ActiveProfile

// Caller-identity note: this daemon registers on the D-Bus session bus,
// whose unix socket lives in /run/user/$UID/bus and is created with
// owner-only permissions. zbus also negotiates the EXTERNAL auth
// mechanism on connect, which verifies peer credentials via SO_PEERCRED.
// Together these mean only processes running as the same UID as the
// daemon can reach our methods — a redundant UID check inside each
// method would not add real protection. We intentionally rely on this.

mod command;
use command::BoundedCommand;

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use zbus::interface;

const SERVICE_NAME: &str = "com.bigcommunity.BigSound1";
const OBJECT_PATH: &str = "/com/bigcommunity/BigSound1";

const RE_PUSH_INTERVAL: Duration = Duration::from_secs(3);

/// Public alias → (internal LADSPA control IDs, default value).
///
/// Defaults match the **BigSound** profile (balanced/hi-fi friendly) —
/// same values as `crates/daemon/data/profiles/00-default.json` and the
/// filter-chain config template. Keeping these three sources in sync is
/// what makes `bigsound show` on a fresh install report numbers that
/// match what PipeWire is actually doing.
const PARAMS: &[(&str, &[&str], f64)] = &[
    (
        "bigbass:target_freq",
        &["bigbass_l:target_freq", "bigbass_r:target_freq"],
        90.0,
    ),
    (
        "bigbass:drive",
        &["bigbass_l:drive", "bigbass_r:drive"],
        0.45,
    ),
    ("bigbass:mix", &["bigbass_l:mix", "bigbass_r:mix"], 0.35),
    (
        "bigbass:cut_dry_lows",
        &["bigbass_l:cut_dry_lows", "bigbass_r:cut_dry_lows"],
        0.0,
    ),
    (
        "bigbass:loudness_db",
        &["bigbass_l:loudness_db", "bigbass_r:loudness_db"],
        2.5,
    ),
    (
        "bigclarity:target_freq",
        &["bigclarity_l:target_freq", "bigclarity_r:target_freq"],
        4000.0,
    ),
    (
        "bigclarity:drive",
        &["bigclarity_l:drive", "bigclarity_r:drive"],
        0.3,
    ),
    (
        "bigclarity:mix",
        &["bigclarity_l:mix", "bigclarity_r:mix"],
        0.2,
    ),
    ("bigspace:width", &["bigspace:width"], 1.2),
    ("bigspace:bass_keep_hz", &["bigspace:bass_keep_hz"], 150.0),
    ("bigspace:mix", &["bigspace:mix"], 1.0),
    ("bigcross:amount", &["bigcross:amount"], 0.3),
    ("bigcross:cutoff_hz", &["bigcross:cutoff_hz"], 700.0),
    ("bigcross:delay_us", &["bigcross:delay_us"], 280.0),
    ("bigloud:amount", &["bigloud:amount"], 0.4),
    ("bigloud:ceiling_db", &["bigloud:ceiling_db"], -1.0),
    ("bigloud:mix", &["bigloud:mix"], 1.0),
];

fn resolve_internal(name: &str) -> Option<&'static [&'static str]> {
    PARAMS
        .iter()
        .find(|(public, _, _)| *public == name)
        .map(|(_, internals, _)| *internals)
}

/// Profile = a named bundle of parameter values plus a list of regex
/// patterns matched against the system default-sink name. When the
/// default sink changes, the daemon picks the first profile (in lexical
/// profile-name order) whose match patterns hit, and applies its params.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Profile {
    name: String,
    #[serde(default)]
    description: String,
    /// Empty list = profile is never auto-applied (manual only).
    #[serde(default)]
    match_patterns: Vec<String>,
    params: HashMap<String, f64>,
}

/// `$XDG_CONFIG_HOME` (or `~/.config`) — the root under which all of
/// BigSound's per-user state lives.
fn config_base() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        })
}

fn user_profiles_dir() -> PathBuf {
    config_base().join("bigsound").join("profiles")
}

/// File holding the user's chosen output device (the real sink BigSound
/// plays through). Empty/absent = automatic (follow WirePlumber priority).
fn output_device_state_path() -> PathBuf {
    config_base().join("bigsound").join("output-device")
}

/// Bounded, non-blocking read of a regular file; do not follow symlinks.
fn read_regular_file(path: &std::path::Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(std::io::Error::other("not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::other("file exceeds limit"));
    }
    Ok(bytes)
}

/// Load the persisted output-device choice. `None` = automatic.
fn load_output_device() -> Option<String> {
    let s = String::from_utf8(read_regular_file(&output_device_state_path(), 256).ok()?).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Persist the output-device choice, reporting errors. `None` writes an empty
/// file meaning "automatic".
fn save_output_device(target: &Option<String>) -> Result<()> {
    let path = output_device_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&path, target.as_deref().unwrap_or("").as_bytes())
        .context("persisting output choice")
}

const SYSTEM_PROFILES_DIR: &str = "/usr/share/bigsound/profiles";

/// Where SaveProfile writes new user-saved profiles.
fn profiles_dir() -> PathBuf {
    user_profiles_dir()
}

/// Parameter ranges match the LADSPA controls, keeping the cache and DSP consistent.
fn parameter_range(name: &str) -> Option<(f64, f64)> {
    Some(match name {
        "bigbass:target_freq" => (40.0, 300.0),
        "bigbass:loudness_db" => (-12.0, 12.0),
        "bigclarity:target_freq" => (1500.0, 8000.0),
        "bigspace:width" => (0.0, 2.0),
        "bigspace:bass_keep_hz" => (40.0, 400.0),
        "bigcross:cutoff_hz" => (400.0, 1500.0),
        "bigcross:delay_us" => (100.0, 500.0),
        "bigloud:ceiling_db" => (-12.0, 0.0),
        _ if resolve_internal(name).is_some() => (0.0, 1.0),
        _ => return None,
    })
}

fn valid_parameter(name: &str, value: f64) -> bool {
    value.is_finite()
        && parameter_range(name).is_some_and(|(low, high)| (low..=high).contains(&value))
}

fn valid_display_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

fn valid_profile_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ')
}

/// Write beside the destination and rename: crashes cannot leave truncated JSON.
fn atomic_write(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(path.parent().unwrap_or(std::path::Path::new(".")))?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Drop any `params` key that doesn't appear in the canonical PARAMS
/// list, and reject keys that obviously look hostile (control chars,
/// excessively long). Loaded profiles can come from anywhere on disk —
/// validating them up front means a malformed or malicious file just
/// becomes a no-op instead of triggering odd behaviour later in the
/// chain. Drops are logged so the user sees why a key was ignored.
fn sanitise_profile(profile: &mut Profile, source: &std::path::Path) {
    profile.params.retain(|k, v| {
        let valid = !k.is_empty()
            && k.len() <= 64
            && k.chars().all(|c| c.is_ascii_graphic())
            && valid_parameter(k, *v);
        if !valid {
            eprintln!(
                "bigsound-daemon: dropping unknown/malformed param {:?} from {}",
                k,
                source.display()
            );
        }
        valid
    });
}

/// Load every profile (`*.json`) found in the system-wide path
/// (/usr/share/bigsound/profiles/) and the user path
/// (~/.config/bigsound/profiles/), with the user path winning for
/// duplicate names. Result map is keyed by the in-file `name` field.
fn load_profiles() -> HashMap<String, Profile> {
    let mut map = HashMap::new();

    // System profiles first (so user copies override them on conflict).
    for source in [PathBuf::from(SYSTEM_PROFILES_DIR), user_profiles_dir()] {
        let Ok(entries) = std::fs::read_dir(&source) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .take(1024)
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(bytes) = read_regular_file(&path, 65536) else {
                continue;
            };
            match serde_json::from_slice::<Profile>(&bytes) {
                Ok(mut p) => {
                    if !valid_display_name(&p.name) {
                        continue;
                    }
                    sanitise_profile(&mut p, &path);
                    map.insert(p.name.clone(), p);
                }
                Err(e) => eprintln!("bigsound-daemon: ignoring {}: {e}", path.display()),
            }
        }
    }
    map
}

fn find_matching_profile<'a>(
    profiles: &'a HashMap<String, Profile>,
    sink_name: &str,
) -> Option<&'a Profile> {
    // Iterate in alphabetical order of profile name so behaviour is
    // deterministic regardless of HashMap iteration order. The numeric
    // filenames are only used when loading; this lookup orders display names.
    let mut names: Vec<&String> = profiles.keys().collect();
    names.sort();
    for name in names {
        let profile = &profiles[name];
        for pat in &profile.match_patterns {
            if let Ok(re) = Regex::new(pat) {
                if re.is_match(sink_name) {
                    return Some(profile);
                }
            }
        }
    }
    None
}

fn current_default_sink_name() -> Option<String> {
    let out = Command::new("pactl").arg("info").bounded_output().ok()?;
    let txt = String::from_utf8_lossy(&out.stdout);
    for line in txt.lines() {
        if let Some(rest) = line.strip_prefix("Default Sink:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Read the `Active Port` of a sink from `pactl list sinks`. Returns
/// `None` when the sink reports no port (typical for bluez sinks and
/// virtual sinks). The port name is what changes when, e.g., a user
/// plugs headphones into a laptop's 3.5mm jack on a system whose codec
/// keeps the same sink name and only flips the active port underneath.
fn active_port_for_sink(sink_name: &str) -> Option<String> {
    let out = Command::new("pactl")
        .args(["list", "sinks"])
        .bounded_output()
        .ok()?;
    let txt = String::from_utf8_lossy(&out.stdout);
    let mut in_target = false;
    for line in txt.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("Name: ") {
            in_target = rest.trim() == sink_name;
            continue;
        }
        if in_target {
            if let Some(rest) = trimmed.strip_prefix("Active Port: ") {
                let port = rest.trim();
                if port.is_empty() || port == "(null)" {
                    return None;
                }
                return Some(port.to_string());
            }
        }
    }
    None
}

/// The real sink currently receiving `BigSound.output`'s audio, resolved
/// from the live link graph via `pw-link -l` (~4 ms, no JSON). This is
/// what profile matching must look at in the normal topology where the
/// system default sink IS BigSound: plugging headphones or connecting a
/// Bluetooth speaker doesn't change the default sink — it changes which
/// device WirePlumber links our playback stream to.
///
/// Output format parsed:
///   alsa_output.pci-....analog-stereo:playback_FL
///     |<- BigSound.output:output_FL
/// i.e. an unindented target port line, followed by indented source lines.
fn real_sink_behind_output() -> Option<String> {
    let out = Command::new("pw-link")
        .args(["-l", "BigSound.output"])
        .bounded_output()
        .ok()?;
    let txt = String::from_utf8_lossy(&out.stdout);
    let mut current_target: Option<&str> = None;
    for line in txt.lines() {
        if !line.starts_with(' ') {
            current_target = Some(line.trim());
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("|<- ") {
            if rest.trim().starts_with("BigSound.output:") {
                // Port is the suffix after the last ':'; node names don't
                // contain ':' in practice.
                if let Some(target) = current_target {
                    let node = target.rsplit_once(':').map(|(n, _)| n).unwrap_or(target);
                    if !node.is_empty() && node != "BigSound" {
                        return Some(node.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Composite identifier we match profile regex against: `<sink>::<port>`
/// when the sink reports a port, otherwise just `<sink>`. This lets a
/// single laptop sink (e.g. `alsa_output.pci-...analog-stereo`) yield
/// different match strings depending on which physical jack is active —
/// the only way to detect headphone-vs-speaker on machines whose codec
/// doesn't rename the sink on insertion.
///
/// When the system default sink is BigSound itself (the normal setup),
/// the identifier is derived from the real device behind
/// `BigSound.output` instead — see [`real_sink_behind_output`].
fn current_default_sink_id() -> Option<String> {
    let mut sink = current_default_sink_name()?;
    if sink == "BigSound" {
        sink = real_sink_behind_output()?;
    }
    match active_port_for_sink(&sink) {
        Some(port) => Some(format!("{sink}::{port}")),
        None => Some(sink),
    }
}

/// Resolve a filter-chain node's current PipeWire id by its `node.name`.
/// Both the `BigSound` sink and its `BigSound.output` playback stream get
/// fresh ids every time filter-chain.service restarts, so callers must be
/// ready to re-resolve.
fn discover_node_id(node_name: &str) -> Result<u32> {
    Ok(discover_node_target(node_name)?.id)
}

fn pipewire_dump() -> Result<JsonValue> {
    let output = Command::new("pw-dump")
        .bounded_output()
        .context("running pw-dump (PipeWire installed?)")?;
    if !output.status.success() {
        bail!(
            "pw-dump failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parsing pw-dump JSON")
}

fn discover_bigsound_node_id() -> Result<u32> {
    discover_node_id("BigSound")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NodeTarget {
    id: u32,
    serial: u32,
}

/// Resolve the runtime id and stable object serial for a node. WirePlumber's
/// move metadata is keyed by the stream's runtime id, while `target.object`
/// must contain the target node's object serial.
fn discover_node_target(node_name: &str) -> Result<NodeTarget> {
    let json = pipewire_dump()?;
    node_target_from_dump(&json, node_name)
}

fn node_target_from_dump(dump: &JsonValue, node_name: &str) -> Result<NodeTarget> {
    let arr = dump
        .as_array()
        .context("pw-dump didn't return a JSON array")?;
    for obj in arr {
        if obj.get("type").and_then(JsonValue::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = obj.pointer("/info/props");
        if props
            .and_then(|p| p.get("node.name"))
            .and_then(JsonValue::as_str)
            != Some(node_name)
        {
            continue;
        }
        let id = obj
            .get("id")
            .and_then(JsonValue::as_u64)
            .with_context(|| format!("{node_name} node has no id"))?;
        let serial = props
            .and_then(|p| p.get("object.serial"))
            .and_then(JsonValue::as_u64)
            .with_context(|| format!("{node_name} node has no object.serial"))?;
        return Ok(NodeTarget {
            id: u32::try_from(id)
                .with_context(|| format!("{node_name} node id is out of range"))?,
            serial: u32::try_from(serial)
                .with_context(|| format!("{node_name} object.serial is out of range"))?,
        });
    }
    bail!(
        "{node_name} node not found — make sure filter-chain.service is running \
         (systemctl --user status filter-chain.service)"
    )
}

/// Cheap liveness check: does PipeWire object `id` still exist and carry
/// the expected `node.name`? Uses `pw-cli info <id>` (one object, a few
/// hundred bytes) instead of `pw-dump` (the whole graph as JSON, easily
/// hundreds of KB) so it's safe to call from the 3 s background tick.
fn node_info_matches(info: &str, node_name: &str, serial: Option<u32>) -> bool {
    let property = |key: &str| {
        info.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim().trim_start_matches('*').trim() == key)
                .then(|| value.trim().trim_matches('"'))
        })
    };
    property("node.name") == Some(node_name)
        && serial.is_none_or(|expected| {
            property("object.serial").and_then(|s| s.parse::<u32>().ok()) == Some(expected)
        })
}

fn node_id_matches(id: u32, node_name: &str, serial: Option<u32>) -> bool {
    if id == 0 {
        return false;
    }
    let Ok(out) = Command::new("pw-cli")
        .args(["info", &id.to_string()])
        .bounded_output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    node_info_matches(&String::from_utf8_lossy(&out.stdout), node_name, serial)
}

/// Whether the live PipeWire graph currently routes `BigSound.output` to
/// `sink_name`. A configured metadata target is only a preference: session
/// policy may move an already-linked stream later (for example to a
/// high-priority USB microphone playback endpoint). Checking the actual link
/// prevents the daemon from mistaking a live output node for a correctly
/// routed one.
fn output_links_to_sink(sink_name: &str) -> bool {
    let Ok(output) = Command::new("pw-link")
        .args(["-l", "BigSound.output"])
        .bounded_output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    output_links_to_sink_from_text(&String::from_utf8_lossy(&output.stdout), sink_name)
}

fn output_links_to_sink_from_text(links: &str, sink_name: &str) -> bool {
    let mut target_matches = false;
    for line in links.lines() {
        if !line.starts_with(' ') {
            let target = line.trim();
            target_matches = target
                .rsplit_once(':')
                .map(|(node, _)| node == sink_name)
                .unwrap_or(false);
            continue;
        }
        if target_matches
            && line
                .trim_start()
                .strip_prefix("|<- ")
                .is_some_and(|source| source.starts_with("BigSound.output:"))
        {
            return true;
        }
    }
    false
}

/// Move the already-live playback stream immediately. PipeWire metadata pins
/// guide WirePlumber when a stream is linked, but changing metadata alone does
/// not reliably detach an existing link. The Pulse compatibility API exposes
/// PipeWire's `object.serial` as the sink-input index, so this performs the
/// same move users make in pavucontrol/wiremix.
fn move_output_stream(out_serial: u32, sink_name: &str) -> Result<()> {
    let output = Command::new("pactl")
        .args(["move-sink-input", &out_serial.to_string(), sink_name])
        .bounded_output()
        .context("running pactl move-sink-input")?;
    if !output.status.success() {
        bail!(
            "pactl move-sink-input failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Enumerate the real output sinks the user could route BigSound through —
/// every `Audio/Sink` except BigSound's own virtual sink. Returns
/// `(node.name, human description)` pairs sorted by description so the GUI
/// dropdown is stable. `node.name` is the stable identifier we pin
/// `target.object` to; the description is what the user sees.
fn list_real_output_sinks() -> Vec<(String, String)> {
    let Ok(output) = Command::new("pw-dump").bounded_output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(json) = serde_json::from_slice::<JsonValue>(&output.stdout) else {
        return Vec::new();
    };
    let Some(arr) = json.as_array() else {
        return Vec::new();
    };
    let mut sinks = Vec::new();
    for obj in arr {
        if obj.get("type").and_then(JsonValue::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = obj.pointer("/info/props") else {
            continue;
        };
        if props.get("media.class").and_then(JsonValue::as_str) != Some("Audio/Sink") {
            continue;
        }
        let name = props
            .get("node.name")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        // Skip empty names and our own virtual sink (routing BigSound into
        // itself would be a loop).
        if name.is_empty() || name == "BigSound" {
            continue;
        }
        let desc = props
            .get("node.description")
            .and_then(JsonValue::as_str)
            .or_else(|| props.get("node.nick").and_then(JsonValue::as_str))
            .unwrap_or(name);
        sinks.push((name.to_string(), desc.to_string()));
    }
    sinks.sort_by(|a, b| a.1.cmp(&b.1));
    sinks
}

/// Pin (or clear) the real sink that `BigSound.output` feeds, by writing
/// `target.object` into PipeWire's `default` metadata — the same lever
/// `wpctl` / `pavucontrol` use to move a stream. `Some(serial)` forces the
/// DSP output to that sink regardless of session priority, so a freshly
/// plugged high-priority USB gadget (e.g. a microphone that also exposes a
/// playback endpoint) can't steal the routing. `None` deletes the pin and
/// lets WirePlumber auto-route by priority again.
fn set_output_target(out_id: u32, target_serial: Option<u32>) -> Result<()> {
    for update in output_target_metadata(target_serial) {
        set_metadata(
            out_id,
            update.key,
            update.value.as_deref(),
            update.value_type,
        )?;
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct MetadataUpdate {
    key: &'static str,
    value: Option<String>,
    value_type: &'static str,
}

fn output_target_metadata(target_serial: Option<u32>) -> [MetadataUpdate; 2] {
    [
        MetadataUpdate {
            key: "target.object",
            value: target_serial.map(|serial| serial.to_string()),
            value_type: "Spa:Id",
        },
        // target.object takes precedence, but an older target.node may still
        // point elsewhere after a previous move. Removing it also ensures
        // Automatic has no legacy pin left behind.
        MetadataUpdate {
            key: "target.node",
            value: None,
            value_type: "Spa:Id",
        },
    ]
}

fn set_metadata(out_id: u32, key: &str, value: Option<&str>, value_type: &str) -> Result<()> {
    let mut cmd = Command::new("pw-metadata");
    cmd.args(["-n", "default"]);
    match value {
        Some(value) => {
            cmd.arg(out_id.to_string())
                .arg(key)
                .arg(value)
                .arg(value_type);
        }
        None => {
            cmd.arg("-d").arg(out_id.to_string()).arg(key);
        }
    }
    let output = cmd.bounded_output().context("running pw-metadata")?;
    if !output.status.success() {
        bail!(
            "pw-metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// One PipeWire update for the complete stereo control transaction.
fn control_pod(values: &[(&str, f64)]) -> Result<String> {
    use std::fmt::Write;
    let mut pod = String::from("{ params = [ ");
    for &(internal, value) in values {
        if !value.is_finite() || !PARAMS.iter().any(|(_, names, _)| names.contains(&internal)) {
            bail!("invalid internal control or non-finite value");
        }
        write!(&mut pod, "\"{internal}\" {value} ")?;
    }
    pod.push_str("] }");
    Ok(pod)
}

fn push_internal_values(node_id: u32, values: &[(&str, f64)]) -> Result<()> {
    let pod = control_pod(values)?;
    let output = Command::new("pw-cli")
        .args(["set-param", &node_id.to_string(), "Props", &pod])
        .bounded_output()
        .context("running pw-cli set-param")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || stderr.contains("Error:") || stdout.contains("Error:") {
        bail!(
            "pw-cli set-param failed: {}",
            if !stderr.trim().is_empty() {
                stderr
            } else {
                stdout
            }
        );
    }
    Ok(())
}

fn push_with_refresh(node_id_atom: &AtomicU32, values: &[(&str, f64)]) -> Result<()> {
    let nid = node_id_atom.load(Ordering::Acquire);
    match push_internal_values(nid, values) {
        Ok(()) => Ok(()),
        Err(e) => match discover_bigsound_node_id() {
            Ok(fresh) if fresh != nid => {
                node_id_atom.store(fresh, Ordering::Release);
                push_internal_values(fresh, values)
            }
            _ => Err(e),
        },
    }
}

// pw-cli 1.0.x builds set-param PODs in a fixed 1024-byte buffer.
// Sixteen known name/value pairs fit with header/alignment overhead. Keep
// each public control's stereo aliases in the same batch.
const MAX_CONTROLS_PER_BATCH: usize = 16;

fn cached_control_batches(cache: &HashMap<String, f64>) -> Vec<Vec<(&'static str, f64)>> {
    let mut batches = Vec::new();
    let mut values = Vec::new();
    for (public, internals, _) in PARAMS {
        if let Some(&value) = cache.get(*public) {
            if values.len() + internals.len() > MAX_CONTROLS_PER_BATCH {
                batches.push(std::mem::take(&mut values));
            }
            for internal in *internals {
                values.push((*internal, value));
            }
        }
    }
    if !values.is_empty() {
        batches.push(values);
    }
    batches
}

fn push_cache_to_pipewire(node_id_atom: &AtomicU32, cache: &HashMap<String, f64>) -> Result<()> {
    for values in cached_control_batches(cache) {
        push_with_refresh(node_id_atom, &values)?;
    }
    Ok(())
}

struct ServiceInner {
    /// Serialize cache changes and pushes, including background retries.
    updates: Mutex<()>,
    node_id: AtomicU32,
    node_serial: AtomicU32,
    cache: Mutex<HashMap<String, f64>>,
    /// Bumped on every cache mutation. The background thread compares
    /// against `last_pushed_gen` to skip re-pushing when nothing has
    /// changed since the previous push — closes the race where an idle
    /// re-push tick could overwrite a fresh user-driven Set call.
    cache_gen: AtomicU64,
    profiles: Mutex<HashMap<String, Profile>>,
    active_profile: Mutex<Option<String>>,
    /// Track the last default sink we saw so the polling thread only
    /// applies a profile when the device actually changes — never on
    /// every tick.
    last_default_sink: Mutex<Option<String>>,
    /// Last resolved id of the `BigSound.output` playback stream (the node
    /// we pin `target.object` on). Re-resolved when a filter-chain restart
    /// hands it a fresh id. 0 = unknown.
    output_node_id: AtomicU32,
    /// The real sink the user chose to route BigSound through. `None` =
    /// automatic (let WirePlumber pick by priority). Persisted to disk.
    output_device: Mutex<Option<String>>,
}

impl ServiceInner {
    fn new(node: NodeTarget) -> Self {
        let mut cache = HashMap::with_capacity(PARAMS.len());
        for (name, _, default) in PARAMS {
            cache.insert((*name).to_string(), *default);
        }
        Self {
            updates: Mutex::new(()),
            node_id: AtomicU32::new(node.id),
            node_serial: AtomicU32::new(node.serial),
            cache: Mutex::new(cache),
            cache_gen: AtomicU64::new(0),
            profiles: Mutex::new(load_profiles()),
            active_profile: Mutex::new(None),
            last_default_sink: Mutex::new(None),
            output_node_id: AtomicU32::new(0),
            output_device: Mutex::new(load_output_device()),
        }
    }

    /// Enforce the configured output target on `BigSound.output`. When a
    /// device is pinned this resolves the (restart-volatile) node id and
    /// asserts the `target.object` metadata, so the choice survives
    /// filter-chain restarts and suspend→resume. When set to automatic it's
    /// a cheap no-op — the pin is cleared at the moment the user switches to
    /// automatic, and a fresh `BigSound.output` starts unpinned anyway.
    ///
    /// `force = false` checks both node identity and live routing before
    /// returning. A configured pin is reasserted if another stream policy
    /// moves the playback link. `force = true` always reapplies the choice.
    fn enforce_output_device(&self, force: bool) -> Result<()> {
        let target = self
            .output_device
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(sink) = target else {
            return Ok(());
        };
        if !force {
            let cached = self.output_node_id.load(Ordering::Acquire);
            if node_id_matches(cached, "BigSound.output", None) && output_links_to_sink(&sink) {
                return Ok(());
            }
        }
        let dump = pipewire_dump()?;
        let output = node_target_from_dump(&dump, "BigSound.output")?;
        let target = node_target_from_dump(&dump, &sink)
            .with_context(|| format!("resolving output device '{sink}'"))?;
        // Move the current live link first, then leave a native PipeWire
        // metadata pin behind for subsequent reconnects/re-creations.
        move_output_stream(output.serial, &sink)
            .with_context(|| format!("moving output to '{sink}'"))?;
        set_output_target(output.id, Some(target.serial))
            .with_context(|| format!("pinning output to '{sink}'"))?;
        self.output_node_id.store(output.id, Ordering::Release);
        Ok(())
    }

    /// Bump the cache generation counter — call this anywhere we mutate
    /// the cache so the background thread knows there's new state to push.
    fn bump_cache_gen(&self) {
        self.cache_gen.fetch_add(1, Ordering::Release);
    }

    fn apply_profile_inner(&self, profile: &Profile) -> Result<()> {
        let _update = self.updates.lock().unwrap_or_else(|e| e.into_inner());
        // Recover from a poisoned mutex instead of panicking — a panic
        // in any other handler shouldn't permanently disable the daemon.
        let previous = self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut snapshot = previous.clone();
        for (k, v) in &profile.params {
            snapshot.insert(k.clone(), *v);
        }
        // Commit the visible state only after PipeWire accepts the update.
        // A failed request must not be applied later by a background retry.
        if let Err(update_error) = push_cache_to_pipewire(&self.node_id, &snapshot) {
            // A later batch can fail after earlier controls changed. Restore
            // the complete confirmed state before rejecting the profile.
            if let Err(rollback_error) = push_cache_to_pipewire(&self.node_id, &previous) {
                // Retry only the previous confirmed cache, never the rejected
                // profile, when the graph becomes available again.
                self.bump_cache_gen();
                bail!("profile update failed: {update_error}; restoring previous controls failed: {rollback_error}; confirmed controls will be retried");
            }
            return Err(update_error.context("profile update rejected; previous controls restored"));
        }
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
        self.bump_cache_gen();
        *self
            .active_profile
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(profile.name.clone());
        Ok(())
    }
}

#[derive(Clone)]
struct BigSoundService {
    inner: Arc<ServiceInner>,
}

#[interface(name = "com.bigcommunity.BigSound1")]
impl BigSoundService {
    fn set(&self, name: &str, value: f64) -> zbus::fdo::Result<()> {
        let _update = self.inner.updates.lock().unwrap_or_else(|e| e.into_inner());
        if !valid_parameter(name, value) {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "unknown parameter or value outside its finite range: '{name}'"
            )));
        }
        let internals = resolve_internal(name)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown parameter '{name}'")))?;
        let values: Vec<_> = internals
            .iter()
            .map(|&internal| (internal, value))
            .collect();
        push_with_refresh(&self.inner.node_id, &values)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.inner
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), value);
        self.inner.bump_cache_gen();
        Ok(())
    }

    fn get(&self, name: &str) -> zbus::fdo::Result<f64> {
        if resolve_internal(name).is_none() {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "unknown parameter '{name}'"
            )));
        }
        let cache = self
            .inner
            .cache
            .lock()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        cache
            .get(name)
            .copied()
            .ok_or_else(|| zbus::fdo::Error::Failed(format!("'{name}' missing from cache")))
    }

    fn list(&self) -> zbus::fdo::Result<Vec<String>> {
        Ok(PARAMS.iter().map(|(name, _, _)| name.to_string()).collect())
    }

    fn list_profiles(&self) -> zbus::fdo::Result<Vec<String>> {
        let profiles = self
            .inner
            .profiles
            .lock()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let mut names: Vec<String> = profiles.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    fn get_profile(&self, name: &str) -> zbus::fdo::Result<String> {
        let profiles = self
            .inner
            .profiles
            .lock()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let profile = profiles
            .get(name)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no such profile '{name}'")))?;
        serde_json::to_string_pretty(profile).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    fn apply_profile(&self, name: &str) -> zbus::fdo::Result<()> {
        let profile = {
            let profiles = self
                .inner
                .profiles
                .lock()
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            profiles
                .get(name)
                .cloned()
                .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no such profile '{name}'")))?
        };
        self.inner
            .apply_profile_inner(&profile)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    fn save_profile(&self, name: &str) -> zbus::fdo::Result<()> {
        // Tight allowlist — alphanumeric, dash, underscore, space — so
        // Unicode lookalikes (∕, /, NUL bytes) can't smuggle in a path
        // separator. Caps profile-name length too so a malicious client
        // can't fill up disk via long filenames.
        let valid_name = valid_profile_name(name);
        if !valid_name {
            return Err(zbus::fdo::Error::InvalidArgs(
                "profile name must be 1..=64 ASCII chars: alphanumeric, '-', '_', or space".into(),
            ));
        }
        let cache = self
            .inner
            .cache
            .lock()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let profile = Profile {
            name: name.to_string(),
            description: "User-saved profile".to_string(),
            match_patterns: Vec::new(),
            params: cache.clone(),
        };
        drop(cache);

        let dir = profiles_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| zbus::fdo::Error::Failed(format!("creating {}: {e}", dir.display())))?;
        let path = dir.join(format!("99-user-{name}.json"));
        let json = serde_json::to_string_pretty(&profile)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        atomic_write(&path, json.as_bytes())
            .map_err(|e| zbus::fdo::Error::Failed(format!("writing {}: {e}", path.display())))?;

        // Refresh the in-memory profiles map.
        if let Ok(mut profiles) = self.inner.profiles.lock() {
            profiles.insert(profile.name.clone(), profile);
        }
        Ok(())
    }

    fn delete_profile(&self, name: &str) -> zbus::fdo::Result<()> {
        // Same allowlist as save_profile — defence in depth even though
        // the lookup is constrained to 99-user-<name>.json.
        let valid_name = valid_profile_name(name);
        if !valid_name {
            return Err(zbus::fdo::Error::InvalidArgs(
                "profile name must be 1..=64 ASCII chars: alphanumeric, '-', '_', or space".into(),
            ));
        }
        // Only delete user profiles (the `99-user-*.json` ones we write).
        let path = profiles_dir().join(format!("99-user-{name}.json"));
        if !path.exists() {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "no user profile '{name}' (built-in profiles can't be deleted)"
            )));
        }
        std::fs::remove_file(&path)
            .map_err(|e| zbus::fdo::Error::Failed(format!("removing {}: {e}", path.display())))?;
        if let Ok(mut profiles) = self.inner.profiles.lock() {
            profiles.remove(name);
        }
        Ok(())
    }

    /// Real output sinks BigSound can be routed through, as
    /// `(node.name, description)` pairs. The GUI uses `node.name` as the
    /// `SetOutputDevice` argument and shows `description` to the user.
    fn list_output_devices(&self) -> zbus::fdo::Result<Vec<(String, String)>> {
        Ok(list_real_output_sinks())
    }

    /// The currently chosen output device (`node.name`), or "" for
    /// automatic (follow WirePlumber priority).
    fn get_output_device(&self) -> zbus::fdo::Result<String> {
        Ok(self
            .inner
            .output_device
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default())
    }

    /// Choose the real sink BigSound's DSP output feeds. Pass "" / "auto"
    /// to return to automatic routing, or a sink `node.name` to pin it
    /// there (overrides session priority so a high-priority USB gadget
    /// can't steal the routing). The choice is persisted and re-asserted
    /// across filter-chain restarts.
    fn set_output_device(&self, name: &str) -> zbus::fdo::Result<()> {
        let trimmed = name.trim();
        let new_target: Option<String> = if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("auto")
            || trimmed.eq_ignore_ascii_case("automatic")
        {
            None
        } else {
            // Light validation: a sink node.name is printable ASCII, never
            // our own virtual sink, and bounded so a hostile client can't
            // smuggle control chars into the metadata pod. `"` and `\` are
            // also rejected — set_output_target wraps the name in a quoted
            // JSON string for pw-metadata, and real node.names never
            // contain either.
            let ok = trimmed.len() <= 256
                && trimmed != "BigSound"
                && trimmed != "BigSound.output"
                && trimmed
                    .chars()
                    .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\');
            if !ok {
                return Err(zbus::fdo::Error::InvalidArgs(format!(
                    "invalid sink name '{name}'"
                )));
            }
            Some(trimmed.to_string())
        };

        let previous_target = {
            let mut output_device = self
                .inner
                .output_device
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let previous = output_device.clone();
            *output_device = new_target.clone();
            previous
        };

        let apply_result = match &new_target {
            Some(_) => self.inner.enforce_output_device(true),
            None => {
                // Switching to automatic: delete any pin on the current
                // output node so WirePlumber resumes priority routing.
                discover_node_id("BigSound.output").and_then(|out_id| {
                    set_output_target(out_id, None)?;
                    self.inner.output_node_id.store(out_id, Ordering::Release);
                    Ok(())
                })
            }
        };

        if let Err(error) = apply_result {
            *self
                .inner
                .output_device
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = previous_target;
            return Err(zbus::fdo::Error::Failed(error.to_string()));
        }

        save_output_device(&new_target).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    #[zbus(property)]
    fn node_id(&self) -> u32 {
        self.inner.node_id.load(Ordering::Acquire)
    }

    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.inner
            .active_profile
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_default()
    }
}

/// Background thread: periodically (a) re-push the cache to PipeWire so
/// it survives suspend→resume of the BigSound sink — only when the cache
/// has actually changed since the last push, gated by `cache_gen` — and
/// (b) check whether the system default sink changed and, if so,
/// auto-apply the matching profile.
fn run_background(inner: Arc<ServiceInner>) {
    // Tracks the cache generation we last pushed so we can skip re-pushing
    // when nothing has changed since. Read the generation BEFORE snapshotting
    // — any Set that lands between the read and the snapshot will already
    // be in the snapshot (harmless redundant push) and will bump the gen
    // again, so the next tick still triggers.
    let mut last_pushed_gen: u64 = 0;
    loop {
        thread::sleep(RE_PUSH_INTERVAL);

        // (a0) Liveness check on the cached BigSound node id. A filter-chain
        // restart spawns a fresh node with **template-default controls** and
        // may recycle our old id for an unrelated object — set-param against
        // it either errors in unpredictable wording or, worse, lands on the
        // wrong node and "succeeds". One cheap `pw-cli info` per tick detects
        // both; on mismatch we re-discover and force a full cache re-push so
        // the chain snaps back to the user's tuning within one tick.
        let mut force_repush = false;
        let nid = inner.node_id.load(Ordering::Acquire);
        if !node_id_matches(
            nid,
            "BigSound",
            Some(inner.node_serial.load(Ordering::Acquire)),
        ) {
            match discover_node_target("BigSound") {
                Ok(fresh) => {
                    eprintln!(
                        "bigsound-daemon: BigSound node identity changed from {nid} (filter-chain restarted?); re-pushing tuning"
                    );
                    inner.node_id.store(fresh.id, Ordering::Release);
                    inner.node_serial.store(fresh.serial, Ordering::Release);
                    force_repush = true;
                }
                Err(e) => {
                    eprintln!("bigsound-daemon: BigSound node unavailable: {e}");
                    continue;
                }
            }
        }

        // (a) Re-push cache when it changed since the last push — or
        // unconditionally right after a filter-chain restart (see a0).
        let current_gen = inner.cache_gen.load(Ordering::Acquire);
        if force_repush || current_gen != last_pushed_gen {
            let _update = inner.updates.lock().unwrap_or_else(|e| e.into_inner());
            let snapshot = inner
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            match push_cache_to_pipewire(&inner.node_id, &snapshot) {
                Ok(()) => last_pushed_gen = current_gen,
                Err(e) => eprintln!("bigsound-daemon: control retry failed: {e}"),
            }
        }

        // (a2) Keep the chosen output device pinned. Cheap no-op when set
        // to automatic; when a device is pinned, a light liveness check on
        // the cached node id is all that runs in steady state — the full
        // re-discover + re-pin only happens when filter-chain restarts hand
        // BigSound.output a fresh node id (or after suspend→resume).
        if let Err(e) = inner.enforce_output_device(false) {
            eprintln!("bigsound-daemon: enforcing output device failed: {e}");
        }

        // (b) Auto-profile detection.
        let current = match current_default_sink_id() {
            Some(s) => s,
            None => {
                eprintln!("bigsound-daemon: poll: no default sink available");
                continue;
            }
        };

        // current_default_sink_id already resolves "default = BigSound"
        // to the real device behind BigSound.output; this guard only
        // remains as a belt-and-braces against matching our own sink
        // (e.g. a transient state while links are being rebuilt).
        if current.starts_with("BigSound::") || current == "BigSound" {
            continue;
        }

        let mut last = inner
            .last_default_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let unchanged = last.as_deref() == Some(current.as_str());
        if unchanged {
            continue;
        }
        eprintln!(
            "bigsound-daemon: poll: default-sink-id changed: {:?} → '{current}'",
            *last
        );
        *last = Some(current.clone());
        drop(last);

        let profile_to_apply = {
            let profiles = inner.profiles.lock().unwrap_or_else(|e| e.into_inner());
            // Try device-matched profiles first (regex against the
            // `<sink>::<port>` composite). If nothing matches, fall back
            // to the canonical "BigSound" profile — balanced defaults
            // that work on any hardware. This is the v0.7+ contract:
            // unknown device never leaves you without a working tuning.
            find_matching_profile(&profiles, &current)
                .cloned()
                .or_else(|| profiles.get("BigSound").cloned())
        };
        match profile_to_apply {
            Some(profile) => {
                eprintln!(
                    "bigsound-daemon: applying profile '{}' for sink '{current}'",
                    profile.name
                );
                if let Err(e) = inner.apply_profile_inner(&profile) {
                    eprintln!("bigsound-daemon: applying profile failed: {e}");
                }
            }
            None => {
                eprintln!(
                    "bigsound-daemon: no profile matched '{current}' and no BigSound fallback found (skipping)"
                );
            }
        }
    }
}

fn main() -> Result<()> {
    // On login, systemd starts this unit alongside filter-chain.service —
    // `After=` only orders the *exec*, not the moment PipeWire actually
    // publishes the BigSound node. Retry for a while instead of exiting
    // immediately, so a slow filter-chain start (cold boot, Bluetooth
    // session bring-up) doesn't burn through systemd's restart limit.
    const DISCOVER_ATTEMPTS: u32 = 30;
    let mut attempt = 0;
    let node = loop {
        match discover_node_target("BigSound") {
            Ok(id) => break id,
            Err(e) => {
                attempt += 1;
                if attempt >= DISCOVER_ATTEMPTS {
                    return Err(e.context("discovering BigSound sink"));
                }
                eprintln!(
                    "bigsound-daemon: BigSound sink not up yet (attempt {attempt}/{DISCOVER_ATTEMPTS}), retrying in 1s"
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
    };
    eprintln!("bigsound-daemon: BigSound node id = {}", node.id);

    let inner = Arc::new(ServiceInner::new(node));

    // Initial push of cache to PipeWire so the running filter-chain
    // matches our defaults from the get-go.
    {
        let cache = inner.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = push_cache_to_pipewire(&inner.node_id, &cache) {
            eprintln!("bigsound-daemon: initial controls pending retry: {e}");
            inner.bump_cache_gen();
        }
    }

    // Re-assert a persisted output-device pin on startup (no-op when the
    // saved choice is automatic) so the routing matches the user's last
    // choice as soon as the daemon comes up. The cached output node id is
    // still 0 here, so this always does the full discover-and-pin.
    if let Err(e) = inner.enforce_output_device(false) {
        eprintln!("bigsound-daemon: restoring output device failed: {e}");
    }
    eprintln!(
        "bigsound-daemon: loaded {} profile(s); user directory {}",
        inner
            .profiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len(),
        profiles_dir().display()
    );

    // Background re-push + auto-profile thread.
    {
        let inner = Arc::clone(&inner);
        thread::Builder::new()
            .name("bigsound-bg".into())
            .spawn(move || run_background(inner))
            .context("spawning background thread")?;
    }

    let service = BigSoundService { inner };

    let _conn = zbus::blocking::connection::Builder::session()?
        .name(SERVICE_NAME)?
        .serve_at(OBJECT_PATH, service)?
        .build()
        .context("registering on session bus")?;

    eprintln!("bigsound-daemon: serving {SERVICE_NAME} on {OBJECT_PATH}");

    loop {
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_node_id_and_serial_from_pipewire_dump() {
        let dump = json!([
            {
                "id": 7,
                "type": "PipeWire:Interface:Port",
                "info": { "props": { "node.name": "speaker-a", "object.serial": 90 } }
            },
            {
                "id": 42,
                "type": "PipeWire:Interface:Node",
                "info": { "props": { "node.name": "speaker-a", "object.serial": 901 } }
            }
        ]);

        let target = node_target_from_dump(&dump, "speaker-a").unwrap();

        assert_eq!(
            target,
            NodeTarget {
                id: 42,
                serial: 901
            }
        );
    }

    #[test]
    fn rejects_node_without_object_serial() {
        let dump = json!([{
            "id": 42,
            "type": "PipeWire:Interface:Node",
            "info": { "props": { "node.name": "speaker-a" } }
        }]);

        let error = node_target_from_dump(&dump, "speaker-a").unwrap_err();

        assert!(error.to_string().contains("object.serial"));
    }

    #[test]
    fn rejects_object_serial_outside_spa_id_range() {
        let dump = json!([{
            "id": 42,
            "type": "PipeWire:Interface:Node",
            "info": {
                "props": {
                    "node.name": "speaker-a",
                    "object.serial": u64::from(u32::MAX) + 1
                }
            }
        }]);

        let error = node_target_from_dump(&dump, "speaker-a").unwrap_err();

        assert!(error.to_string().contains("object.serial is out of range"));
    }

    #[test]
    fn rejects_missing_node() {
        let dump = json!([]);

        let error = node_target_from_dump(&dump, "speaker-a").unwrap_err();

        assert!(error.to_string().contains("speaker-a node not found"));
    }

    #[test]
    fn pins_output_with_object_serial_as_spa_id() {
        let updates = output_target_metadata(Some(901));

        assert_eq!(
            updates,
            [
                MetadataUpdate {
                    key: "target.object",
                    value: Some("901".to_string()),
                    value_type: "Spa:Id"
                },
                MetadataUpdate {
                    key: "target.node",
                    value: None,
                    value_type: "Spa:Id"
                }
            ]
        );
    }

    #[test]
    fn automatic_output_clears_current_and_legacy_targets() {
        let updates = output_target_metadata(None);

        assert!(updates.iter().all(|update| update.value.is_none()));
        assert_eq!(updates[0].key, "target.object");
        assert_eq!(updates[1].key, "target.node");
    }

    #[test]
    fn detects_configured_sink_in_live_output_links() {
        let links = r#"BigSound.output:output_FL
  |-> wiremix-capture:input_FL
bluez_output.AA_BB.1:playback_FL
  |<- BigSound.output:output_FL
bluez_output.AA_BB.1:playback_FR
  |<- BigSound.output:output_FR
"#;

        assert!(output_links_to_sink_from_text(
            links,
            "bluez_output.AA_BB.1"
        ));
        assert!(!output_links_to_sink_from_text(
            links,
            "alsa_output.usb-microphone"
        ));
    }

    #[test]
    fn rejects_stale_link_to_a_different_sink() {
        let links = r#"alsa_output.usb-microphone:playback_FL
  |<- BigSound.output:output_FL
alsa_output.usb-microphone:playback_FR
  |<- BigSound.output:output_FR
"#;

        assert!(!output_links_to_sink_from_text(
            links,
            "bluez_output.AA_BB.1"
        ));
    }
    #[test]
    fn bundled_profiles_and_defaults_obey_control_ranges() {
        for (name, _, default) in PARAMS {
            assert!(valid_parameter(name, *default), "{name}");
        }
        for entry in
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/data/profiles")).unwrap()
        {
            let profile: Profile =
                serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
            assert!(valid_display_name(&profile.name));
            assert_eq!(profile.params.len(), PARAMS.len());
            for (name, value) in &profile.params {
                assert!(valid_parameter(name, *value), "{}: {name}", profile.name);
            }
            for pattern in &profile.match_patterns {
                assert!(Regex::new(pattern).is_ok());
            }
            if profile.name == "BigSound" {
                for (name, _, default) in PARAMS {
                    assert_eq!(profile.params[*name], *default);
                }
            }
        }
    }

    #[test]
    fn invalid_controls_and_profile_paths_are_rejected() {
        for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(!valid_parameter("bigloud:amount", value));
        }
        assert!(!valid_parameter("unknown", 0.5));
        for name in ["", " ", "../private", "a/b", "a\\b", "a\n", "a∕b"] {
            assert!(!valid_profile_name(name));
        }
        assert!(valid_profile_name("My Mix-2_"));
        assert!(!valid_profile_name(&"a".repeat(65)));
    }

    #[test]
    fn loaded_profile_drops_out_of_range_and_unknown_values() {
        let mut profile = Profile {
            name: "test".into(),
            description: String::new(),
            match_patterns: vec![],
            params: [
                ("bigbass:mix".into(), 0.2),
                ("bigcross:amount".into(), 99.0),
                ("unknown".into(), 0.3),
                ("bigloud:mix".into(), f64::NAN),
            ]
            .into(),
        };
        sanitise_profile(&mut profile, std::path::Path::new("fixture.json"));
        assert_eq!(profile.params.len(), 1);
        assert_eq!(profile.params["bigbass:mix"], 0.2);
    }

    #[test]
    fn control_transaction_contains_both_channels_and_rejects_injection() {
        let pod = control_pod(&[("bigbass_l:mix", 0.2), ("bigbass_r:mix", 0.2)]).unwrap();
        assert_eq!(
            pod,
            "{ params = [ \"bigbass_l:mix\" 0.2 \"bigbass_r:mix\" 0.2 ] }"
        );
        assert!(control_pod(&[("bad\" ] }", 0.2)]).is_err());
        assert!(control_pod(&[("bigbass_l:mix", f64::INFINITY)]).is_err());
    }

    #[test]
    fn profile_batches_fit_legacy_pod_buffer_and_keep_stereo_pairs_together() {
        let cache: HashMap<_, _> = PARAMS
            .iter()
            .map(|(name, _, value)| (name.to_string(), *value))
            .collect();
        let batches = cached_control_batches(&cache);
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches.iter().map(Vec::len).sum::<usize>(),
            PARAMS
                .iter()
                .map(|(_, internals, _)| internals.len())
                .sum::<usize>()
        );
        for batch in &batches {
            // Conservative SPA object/struct header plus padded String and
            // numeric PODs. Numeric PODs need at most sixteen bytes.
            let encoded_bound = 64
                + batch
                    .iter()
                    .map(|(name, _)| 8 + (name.len() + 1).div_ceil(8) * 8 + 16)
                    .sum::<usize>();
            assert!(encoded_bound <= 1024);
            control_pod(batch).unwrap();
        }
        for (_, internals, _) in PARAMS {
            assert!(batches.iter().any(|batch| internals
                .iter()
                .all(|name| batch.iter().any(|(actual, _)| actual == name))));
        }
    }

    #[test]
    fn persistence_replaces_atomically_and_does_not_follow_destination_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir =
            std::env::temp_dir().join(format!("bigsound-persistence-test-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let target = dir.join("profile.json");
        let victim = dir.join("unrelated.json");
        std::fs::write(&victim, b"unchanged").unwrap();
        symlink(&victim, &target).unwrap();
        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second");
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn recycled_node_id_requires_matching_object_serial() {
        let info = "  * node.name = \"BigSound\"\n    object.serial = \"120\"\n";
        assert!(node_info_matches(info, "BigSound", Some(120)));
        assert!(!node_info_matches(info, "BigSound", Some(119)));
        assert!(!node_info_matches(info, "Other", Some(120)));
    }
    #[test]
    fn bounded_file_reads_reject_large_files_and_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("bigsound-read-test-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("profile.json");
        std::fs::write(&path, b"data").unwrap();
        assert_eq!(read_regular_file(&path, 4).unwrap(), b"data");
        assert!(read_regular_file(&path, 3).is_err());
        let link = dir.join("link.json");
        symlink(&path, &link).unwrap();
        assert!(read_regular_file(&link, 10).is_err());
        assert!(read_regular_file(&dir, 10).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
