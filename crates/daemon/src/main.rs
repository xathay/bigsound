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
/// filename order) whose match patterns hit, and applies its params.
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

/// Load the persisted output-device choice. `None` = automatic.
fn load_output_device() -> Option<String> {
    let s = std::fs::read_to_string(output_device_state_path()).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Persist the output-device choice (best-effort). `None` writes an empty
/// file meaning "automatic".
fn save_output_device(target: &Option<String>) {
    let path = output_device_state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, target.as_deref().unwrap_or(""));
}

const SYSTEM_PROFILES_DIR: &str = "/usr/share/bigsound/profiles";

/// Where SaveProfile writes new user-saved profiles.
fn profiles_dir() -> PathBuf {
    user_profiles_dir()
}

/// Drop any `params` key that doesn't appear in the canonical PARAMS
/// list, and reject keys that obviously look hostile (control chars,
/// excessively long). Loaded profiles can come from anywhere on disk —
/// validating them up front means a malformed or malicious file just
/// becomes a no-op instead of triggering odd behaviour later in the
/// chain. Drops are logged so the user sees why a key was ignored.
fn sanitise_profile(profile: &mut Profile, source: &std::path::Path) {
    profile.params.retain(|k, _v| {
        let valid = !k.is_empty()
            && k.len() <= 64
            && k.chars().all(|c| c.is_ascii_graphic())
            && resolve_internal(k).is_some();
        if !valid {
            eprintln!(
                "bigsound-daemon: dropping unknown/malformed param '{}' from {}",
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
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            match serde_json::from_slice::<Profile>(&bytes) {
                Ok(mut p) => {
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
    // prefix convention (00-default, 10-laptop, 20-headphones, ...)
    // gives a sensible priority cascade.
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
    let out = Command::new("pactl").arg("info").output().ok()?;
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
        .output()
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

/// Composite identifier we match profile regex against: `<sink>::<port>`
/// when the sink reports a port, otherwise just `<sink>`. This lets a
/// single laptop sink (e.g. `alsa_output.pci-...analog-stereo`) yield
/// different match strings depending on which physical jack is active —
/// the only way to detect headphone-vs-speaker on machines whose codec
/// doesn't rename the sink on insertion.
fn current_default_sink_id() -> Option<String> {
    let sink = current_default_sink_name()?;
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
    let output = Command::new("pw-dump")
        .output()
        .context("running pw-dump (PipeWire installed?)")?;
    if !output.status.success() {
        bail!(
            "pw-dump failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let json: JsonValue = serde_json::from_slice(&output.stdout).context("parsing pw-dump JSON")?;
    let arr = json
        .as_array()
        .context("pw-dump didn't return a JSON array")?;
    for obj in arr {
        if obj.get("type").and_then(JsonValue::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let name = obj
            .pointer("/info/props/node.name")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if name == node_name {
            return Ok(obj
                .get("id")
                .and_then(JsonValue::as_u64)
                .with_context(|| format!("{node_name} node has no id"))?
                as u32);
        }
    }
    bail!(
        "{node_name} node not found — make sure filter-chain.service is running \
         (systemctl --user status filter-chain.service)"
    )
}

fn discover_bigsound_node_id() -> Result<u32> {
    discover_node_id("BigSound")
}

/// Enumerate the real output sinks the user could route BigSound through —
/// every `Audio/Sink` except BigSound's own virtual sink. Returns
/// `(node.name, human description)` pairs sorted by description so the GUI
/// dropdown is stable. `node.name` is the stable identifier we pin
/// `target.object` to; the description is what the user sees.
fn list_real_output_sinks() -> Vec<(String, String)> {
    let Ok(output) = Command::new("pw-dump").output() else {
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
/// `wpctl` / `pavucontrol` use to move a stream. `Some(sink)` forces the
/// DSP output to that sink regardless of session priority, so a freshly
/// plugged high-priority USB gadget (e.g. a microphone that also exposes a
/// playback endpoint) can't steal the routing. `None` deletes the pin and
/// lets WirePlumber auto-route by priority again.
fn set_output_target(out_id: u32, target: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("pw-metadata");
    cmd.args(["-n", "default"]);
    match target {
        Some(sink) => {
            cmd.arg(out_id.to_string())
                .arg("target.object")
                .arg(format!("\"{sink}\""))
                .arg("Spa:String:JSON");
        }
        None => {
            cmd.arg("-d").arg(out_id.to_string()).arg("target.object");
        }
    }
    let output = cmd.output().context("running pw-metadata")?;
    if !output.status.success() {
        bail!(
            "pw-metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn push_internal_value(node_id: u32, internal: &str, value: f64) -> Result<()> {
    // Reject non-finite values up front — they'd serialize as the
    // literal `NaN`/`inf` token in the SPA pod, which pw-cli rejects
    // and which would, in a worst case, escape into shell-interpretable
    // tokens if pw-cli's pod parser ever drops to a permissive mode.
    if !value.is_finite() {
        bail!(
            "refusing to push non-finite value {} for {}",
            value,
            internal
        );
    }
    let pod = format!("{{ params = [ \"{internal}\" {value} ] }}");
    let output = Command::new("pw-cli")
        .args(["set-param", &node_id.to_string(), "Props", &pod])
        .output()
        .context("running pw-cli set-param")?;
    // pw-cli exits 0 even when set-param fails — it just prints
    // `Error: "..."` on stdout/stderr. Treat any such marker as failure
    // so the caller can react (e.g. re-discover the node id).
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || stderr.contains("Error:") || stdout.contains("Error:") {
        bail!(
            "pw-cli set-param failed: {}",
            if !stderr.trim().is_empty() {
                stderr.into_owned()
            } else {
                stdout.into_owned()
            }
        );
    }
    Ok(())
}

/// Push one value, refreshing the cached node id once if pw-cli rejects
/// the target (filter-chain.service restarts hand out new node ids and
/// our cached one goes stale — without this retry, every Set/ApplyProfile
/// silently no-ops until the daemon itself is restarted).
fn push_with_refresh(node_id_atom: &AtomicU32, internal: &str, value: f64) -> Result<()> {
    let nid = node_id_atom.load(Ordering::Acquire);
    match push_internal_value(nid, internal, value) {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            // pw-cli's "no global N" / "unknown" responses indicate the
            // node went away. Anything else (e.g. ENOENT for pw-cli itself)
            // we propagate without thrashing pw-dump.
            let looks_stale = msg.contains("no global")
                || msg.contains("unknown")
                || msg.contains("No such")
                || msg.contains("not found");
            if !looks_stale {
                return Err(e);
            }
            match discover_bigsound_node_id() {
                Ok(fresh) if fresh != nid => {
                    eprintln!(
                        "bigsound-daemon: BigSound node id changed {nid} → {fresh} (filter-chain restarted?); retrying push"
                    );
                    node_id_atom.store(fresh, Ordering::Release);
                    push_internal_value(fresh, internal, value)
                }
                _ => Err(e),
            }
        }
    }
}

/// Push every cache entry to PipeWire (best-effort, silent on failure).
fn push_cache_to_pipewire(node_id_atom: &AtomicU32, cache: &HashMap<String, f64>) {
    for (public, internals, _) in PARAMS {
        let value = match cache.get(*public) {
            Some(v) => *v,
            None => continue,
        };
        for internal in *internals {
            let _ = push_with_refresh(node_id_atom, internal, value);
        }
    }
}

struct ServiceInner {
    node_id: AtomicU32,
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
    fn new(node_id: u32) -> Self {
        let mut cache = HashMap::with_capacity(PARAMS.len());
        for (name, _, default) in PARAMS {
            cache.insert((*name).to_string(), *default);
        }
        Self {
            node_id: AtomicU32::new(node_id),
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
    /// device is pinned this re-resolves the (restart-volatile) node id and
    /// re-asserts the `target.object` metadata, so the choice survives
    /// filter-chain restarts and suspend→resume. When set to automatic it's
    /// a cheap no-op — the pin is cleared at the moment the user switches to
    /// automatic, and a fresh `BigSound.output` starts unpinned anyway.
    fn enforce_output_device(&self) {
        let target = self
            .output_device
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(sink) = target else {
            return;
        };
        let out_id = match discover_node_id("BigSound.output") {
            Ok(id) => id,
            Err(_) => return,
        };
        self.output_node_id.store(out_id, Ordering::Release);
        if let Err(e) = set_output_target(out_id, Some(&sink)) {
            eprintln!("bigsound-daemon: pinning output to '{sink}' failed: {e}");
        }
    }

    /// Bump the cache generation counter — call this anywhere we mutate
    /// the cache so the background thread knows there's new state to push.
    fn bump_cache_gen(&self) {
        self.cache_gen.fetch_add(1, Ordering::Release);
    }

    fn apply_profile_inner(&self, profile: &Profile) {
        // Recover from a poisoned mutex instead of panicking — a panic
        // in any other handler shouldn't permanently disable the daemon.
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        for (k, v) in &profile.params {
            cache.insert(k.clone(), *v);
        }
        // Snapshot for pushing while not holding any other lock.
        let snapshot = cache.clone();
        drop(cache);
        self.bump_cache_gen();
        push_cache_to_pipewire(&self.node_id, &snapshot);
        *self
            .active_profile
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(profile.name.clone());
    }
}

#[derive(Clone)]
struct BigSoundService {
    inner: Arc<ServiceInner>,
}

#[interface(name = "com.bigcommunity.BigSound1")]
impl BigSoundService {
    fn set(&self, name: &str, value: f64) -> zbus::fdo::Result<()> {
        if !value.is_finite() {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "value must be finite (got {value})"
            )));
        }
        let internals = resolve_internal(name)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("unknown parameter '{name}'")))?;
        if let Ok(mut cache) = self.inner.cache.lock() {
            cache.insert(name.to_string(), value);
        }
        self.inner.bump_cache_gen();
        for internal in internals {
            let _ = push_with_refresh(&self.inner.node_id, internal, value);
        }
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
        self.inner.apply_profile_inner(&profile);
        Ok(())
    }

    fn save_profile(&self, name: &str) -> zbus::fdo::Result<()> {
        // Tight allowlist — alphanumeric, dash, underscore, space — so
        // Unicode lookalikes (∕, /, NUL bytes) can't smuggle in a path
        // separator. Caps profile-name length too so a malicious client
        // can't fill up disk via long filenames.
        let valid_name = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ');
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
        std::fs::write(&path, json)
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
        let valid_name = !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ');
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
            // smuggle control chars into the metadata pod.
            let ok = trimmed.len() <= 256
                && trimmed != "BigSound"
                && trimmed.chars().all(|c| c.is_ascii_graphic());
            if !ok {
                return Err(zbus::fdo::Error::InvalidArgs(format!(
                    "invalid sink name '{name}'"
                )));
            }
            Some(trimmed.to_string())
        };

        *self
            .inner
            .output_device
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = new_target.clone();
        save_output_device(&new_target);

        match &new_target {
            Some(_) => self.inner.enforce_output_device(),
            None => {
                // Switching to automatic: delete any pin on the current
                // output node so WirePlumber resumes priority routing.
                if let Ok(out_id) = discover_node_id("BigSound.output") {
                    self.inner.output_node_id.store(out_id, Ordering::Release);
                    let _ = set_output_target(out_id, None);
                }
            }
        }
        Ok(())
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

        // (a) Re-push cache, but only if it changed since the last push.
        let current_gen = inner.cache_gen.load(Ordering::Acquire);
        if current_gen != last_pushed_gen {
            if let Ok(cache) = inner.cache.lock() {
                let snapshot = cache.clone();
                drop(cache);
                push_cache_to_pipewire(&inner.node_id, &snapshot);
                last_pushed_gen = current_gen;
            }
        }

        // (a2) Keep the chosen output device pinned. Cheap no-op when set
        // to automatic; when a device is pinned it re-asserts target.object
        // so the choice survives filter-chain restarts (which hand
        // BigSound.output a fresh node id) and suspend→resume.
        inner.enforce_output_device();

        // (b) Auto-profile detection.
        let current = match current_default_sink_id() {
            Some(s) => s,
            None => {
                eprintln!("bigsound-daemon: poll: no default sink available");
                continue;
            }
        };

        // Don't auto-switch when the default sink IS BigSound itself —
        // we react instead to which *real* device drives BigSound.output.
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
                inner.apply_profile_inner(&profile);
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
    let node_id = discover_bigsound_node_id().context("discovering BigSound sink")?;
    eprintln!("bigsound-daemon: BigSound node id = {node_id}");

    let inner = Arc::new(ServiceInner::new(node_id));

    // Initial push of cache to PipeWire so the running filter-chain
    // matches our defaults from the get-go.
    {
        let cache = inner.cache.lock().unwrap_or_else(|e| e.into_inner());
        push_cache_to_pipewire(&inner.node_id, &cache);
    }

    // Re-assert a persisted output-device pin on startup (no-op when the
    // saved choice is automatic) so the routing matches the user's last
    // choice as soon as the daemon comes up.
    inner.enforce_output_device();
    eprintln!(
        "bigsound-daemon: loaded {} profile(s) from {}",
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
