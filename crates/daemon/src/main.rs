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
//!   property NodeId, ActiveProfile

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
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
    ("bigbass:target_freq", &["bigbass_l:target_freq", "bigbass_r:target_freq"],  90.0),
    ("bigbass:drive",        &["bigbass_l:drive",        "bigbass_r:drive"],       0.45),
    ("bigbass:mix",          &["bigbass_l:mix",          "bigbass_r:mix"],         0.35),
    ("bigbass:cut_dry_lows", &["bigbass_l:cut_dry_lows", "bigbass_r:cut_dry_lows"], 0.0),
    ("bigbass:loudness_db",  &["bigbass_l:loudness_db",  "bigbass_r:loudness_db"], 2.5),
    ("bigclarity:target_freq", &["bigclarity_l:target_freq", "bigclarity_r:target_freq"], 4000.0),
    ("bigclarity:drive",       &["bigclarity_l:drive",       "bigclarity_r:drive"],         0.3),
    ("bigclarity:mix",         &["bigclarity_l:mix",         "bigclarity_r:mix"],           0.2),
    ("bigspace:width",        &["bigspace:width"],          1.2),
    ("bigspace:bass_keep_hz", &["bigspace:bass_keep_hz"], 150.0),
    ("bigspace:mix",          &["bigspace:mix"],            1.0),
    ("bigcross:amount",    &["bigcross:amount"],     0.3),
    ("bigcross:cutoff_hz", &["bigcross:cutoff_hz"], 700.0),
    ("bigcross:delay_us",  &["bigcross:delay_us"],  280.0),
    ("bigloud:amount",     &["bigloud:amount"],      0.4),
    ("bigloud:ceiling_db", &["bigloud:ceiling_db"], -1.0),
    ("bigloud:mix",        &["bigloud:mix"],         1.0),
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

fn user_profiles_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
    base.join("bigsound").join("profiles")
}

const SYSTEM_PROFILES_DIR: &str = "/usr/share/bigsound/profiles";

/// Where SaveProfile writes new user-saved profiles.
fn profiles_dir() -> PathBuf {
    user_profiles_dir()
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
            let Ok(bytes) = std::fs::read(&path) else { continue };
            match serde_json::from_slice::<Profile>(&bytes) {
                Ok(p) => {
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
    let out = Command::new("pactl").args(["list", "sinks"]).output().ok()?;
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

fn discover_bigsound_node_id() -> Result<u32> {
    let output = Command::new("pw-dump")
        .output()
        .context("running pw-dump (PipeWire installed?)")?;
    if !output.status.success() {
        bail!("pw-dump failed: {}", String::from_utf8_lossy(&output.stderr));
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
        if name == "BigSound" {
            return Ok(obj
                .get("id")
                .and_then(JsonValue::as_u64)
                .context("BigSound node has no id")? as u32);
        }
    }
    bail!(
        "BigSound sink not found — make sure filter-chain.service is running \
         (systemctl --user status filter-chain.service)"
    )
}

fn push_internal_value(node_id: u32, internal: &str, value: f64) -> Result<()> {
    let pod = format!("{{ params = [ \"{internal}\" {value} ] }}");
    let output = Command::new("pw-cli")
        .args(["set-param", &node_id.to_string(), "Props", &pod])
        .output()
        .context("running pw-cli set-param")?;
    if !output.status.success() {
        bail!(
            "pw-cli set-param failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Push every cache entry to PipeWire (best-effort, silent on failure).
fn push_cache_to_pipewire(node_id: u32, cache: &HashMap<String, f64>) {
    for (public, internals, _) in PARAMS {
        let value = match cache.get(*public) {
            Some(v) => *v,
            None => continue,
        };
        for internal in *internals {
            let _ = push_internal_value(node_id, internal, value);
        }
    }
}

struct ServiceInner {
    node_id: u32,
    cache: Mutex<HashMap<String, f64>>,
    profiles: Mutex<HashMap<String, Profile>>,
    active_profile: Mutex<Option<String>>,
    /// Track the last default sink we saw so the polling thread only
    /// applies a profile when the device actually changes — never on
    /// every tick.
    last_default_sink: Mutex<Option<String>>,
}

impl ServiceInner {
    fn new(node_id: u32) -> Self {
        let mut cache = HashMap::with_capacity(PARAMS.len());
        for (name, _, default) in PARAMS {
            cache.insert((*name).to_string(), *default);
        }
        Self {
            node_id,
            cache: Mutex::new(cache),
            profiles: Mutex::new(load_profiles()),
            active_profile: Mutex::new(None),
            last_default_sink: Mutex::new(None),
        }
    }

    fn apply_profile_inner(&self, profile: &Profile) {
        let mut cache = self.cache.lock().unwrap();
        for (k, v) in &profile.params {
            cache.insert(k.clone(), *v);
        }
        // Snapshot for pushing while not holding any other lock.
        let snapshot = cache.clone();
        drop(cache);
        push_cache_to_pipewire(self.node_id, &snapshot);
        *self.active_profile.lock().unwrap() = Some(profile.name.clone());
    }
}

#[derive(Clone)]
struct BigSoundService {
    inner: Arc<ServiceInner>,
}

#[interface(name = "com.bigcommunity.BigSound1")]
impl BigSoundService {
    fn set(&self, name: &str, value: f64) -> zbus::fdo::Result<()> {
        let internals = resolve_internal(name).ok_or_else(|| {
            zbus::fdo::Error::InvalidArgs(format!("unknown parameter '{name}'"))
        })?;
        if let Ok(mut cache) = self.inner.cache.lock() {
            cache.insert(name.to_string(), value);
        }
        for internal in internals {
            let _ = push_internal_value(self.inner.node_id, internal, value);
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
        serde_json::to_string_pretty(profile)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
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
        if name.contains('/') || name.contains('\\') || name.is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "profile name must be a non-empty single segment".into(),
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

    #[zbus(property)]
    fn node_id(&self) -> u32 {
        self.inner.node_id
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
/// it survives suspend→resume of the BigSound sink, and (b) check whether
/// the system default sink changed and, if so, auto-apply the matching
/// profile.
fn run_background(inner: Arc<ServiceInner>) {
    loop {
        thread::sleep(RE_PUSH_INTERVAL);

        // (a) Re-push cache.
        if let Ok(cache) = inner.cache.lock() {
            let snapshot = cache.clone();
            drop(cache);
            push_cache_to_pipewire(inner.node_id, &snapshot);
        }

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

        let mut last = inner.last_default_sink.lock().unwrap();
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
            let profiles = inner.profiles.lock().unwrap();
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
        let cache = inner.cache.lock().unwrap();
        push_cache_to_pipewire(node_id, &cache);
    }
    eprintln!(
        "bigsound-daemon: loaded {} profile(s) from {}",
        inner.profiles.lock().unwrap().len(),
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
