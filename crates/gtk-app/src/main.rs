//! BigSound GTK4 + libadwaita frontend.
//!
//! Single window, six sliders driving the BigBass / BigClarity / BigLoud
//! parameters live via the BigSound D-Bus daemon. Slider movement → D-Bus
//! `Set` call → `pw-cli set-param` → audio changes within milliseconds.

use gettextrs::{bind_textdomain_codeset, bindtextdomain, gettext, textdomain};
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use zbus::blocking::{Connection, Proxy};

const APP_ID: &str = "com.bigcommunity.BigSound";
const SVC: &str = "com.bigcommunity.BigSound1";
const PATH: &str = "/com/bigcommunity/BigSound1";
const IFACE: &str = "com.bigcommunity.BigSound1";
const TEXT_DOMAIN: &str = "bigsound";

/// Set up gettext so every `gettext("...")` lookup resolves against the
/// installed `bigsound.mo` for the current locale. We try the user's
/// `~/.local/share/locale` first (where `scripts/install.sh` puts the
/// catalog) and fall back to the system path `/usr/share/locale` (where
/// the PKGBUILD installs it). English literals in the source are the
/// canonical msgid strings — translations live in `crates/gtk-app/po/`.
fn init_i18n() {
    let _ = textdomain(TEXT_DOMAIN);

    let home = std::env::var("HOME").unwrap_or_default();
    let user_dir = format!("{home}/.local/share/locale");
    let user_mo = format!("{user_dir}/pt_BR/LC_MESSAGES/bigsound.mo");

    if std::path::Path::new(&user_mo).exists() {
        let _ = bindtextdomain(TEXT_DOMAIN, &user_dir);
    } else {
        let _ = bindtextdomain(TEXT_DOMAIN, "/usr/share/locale");
    }
    let _ = bind_textdomain_codeset(TEXT_DOMAIN, "UTF-8");
}

/// Shorthand for `gettext` to keep call sites compact.
fn tr(s: &str) -> String {
    gettext(s)
}

/// Cheap-to-clone D-Bus handle. zbus::blocking::Connection is Arc-backed
/// internally; we share one connection across every slider callback so
/// each Set() reuses it instead of opening a new socket.
#[derive(Clone)]
struct Bus(Connection);

impl Bus {
    fn connect() -> anyhow::Result<Self> {
        Ok(Bus(Connection::session()?))
    }

    fn proxy(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(&self.0, SVC, PATH, IFACE)
    }

    fn set(&self, param: &str, value: f64) {
        if let Ok(p) = self.proxy() {
            // Ignore errors silently — slider drag generates many calls
            // and the daemon will catch up. UX-wise, refusing to update
            // the visual when D-Bus hiccups would feel worse than just
            // letting the next call land.
            let _ = p.call::<_, _, ()>("Set", &(param, value));
        }
    }

    fn get(&self, param: &str) -> Option<f64> {
        let p = self.proxy().ok()?;
        p.call("Get", &(param,)).ok()
    }

    fn list_profiles(&self) -> Vec<String> {
        self.proxy()
            .and_then(|p| p.call("ListProfiles", &()))
            .unwrap_or_default()
    }

    /// Pull the human-readable description out of a profile JSON. The
    /// daemon's GetProfile RPC returns the full Profile struct serialised;
    /// we only care about `name` + `description` for the UI badge.
    fn profile_description(&self, name: &str) -> Option<String> {
        let json: String = self.proxy().ok()?.call("GetProfile", &(name,)).ok()?;
        let value: serde_json::Value = serde_json::from_str(&json).ok()?;
        value
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn apply_profile(&self, name: &str) {
        if let Ok(p) = self.proxy() {
            let _ = p.call::<_, _, ()>("ApplyProfile", &(name,));
        }
    }

    fn active_profile(&self) -> Option<String> {
        let p = self.proxy().ok()?;
        let name: String = p.get_property("ActiveProfile").ok()?;
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    /// Real sinks BigSound can be routed through, as `(node.name,
    /// description)` pairs. `node.name` is the stable id passed back to
    /// `SetOutputDevice`; `description` is the human label.
    fn list_output_devices(&self) -> Vec<(String, String)> {
        self.proxy()
            .and_then(|p| p.call("ListOutputDevices", &()))
            .unwrap_or_default()
    }

    /// The currently chosen output sink (`node.name`), or "" for automatic.
    fn output_device(&self) -> String {
        self.proxy()
            .and_then(|p| p.call("GetOutputDevice", &()))
            .unwrap_or_default()
    }

    /// Route BigSound's DSP output to `name` (a sink `node.name`), or pass
    /// "" to return to automatic priority-based routing.
    fn set_output_device(&self, name: &str) {
        if let Ok(p) = self.proxy() {
            let _ = p.call::<_, _, ()>("SetOutputDevice", &(name,));
        }
    }
}

/// Spec for one slider row. `title` and `subtitle` are translatable strings
/// owned at runtime (gettext returns String) so locale changes apply
/// without recompiling. `param` is the canonical D-Bus parameter name and
/// stays English — it's an identifier, not a label.
#[derive(Clone)]
struct SliderSpec {
    title: String,
    subtitle: String,
    param: &'static str,
    min: f64,
    max: f64,
    step: f64,
    digits: i32,
    default: f64,
}

/// Build the list of sliders shown in the main window. Strings go through
/// `gettext` so the user's locale (or the absence of a translation) picks
/// the right rendering. The English literals here are the msgid keys the
/// translator works against.
fn slider_specs() -> Vec<SliderSpec> {
    vec![
        SliderSpec {
            title: tr("Bass"),
            subtitle: tr("Make-up gain on the psychoacoustic bass enhancement"),
            param: "bigbass:loudness_db",
            min: -12.0, max: 12.0, step: 0.5, digits: 1, default: 2.5,
        },
        SliderSpec {
            title: tr("Bass Frequency (Hz)"),
            subtitle: tr("Below this the speaker rolls off; harmonics are synthesised here"),
            param: "bigbass:target_freq",
            min: 40.0, max: 200.0, step: 5.0, digits: 0, default: 90.0,
        },
        SliderSpec {
            title: tr("Bass Mix"),
            subtitle: tr("How much of the synthesised harmonic content to add"),
            param: "bigbass:mix",
            min: 0.0, max: 1.0, step: 0.05, digits: 2, default: 0.35,
        },
        SliderSpec {
            title: tr("Clarity"),
            subtitle: tr("Treble exciter — sparkle on hi-hats, vocals, strings"),
            param: "bigclarity:mix",
            min: 0.0, max: 1.0, step: 0.05, digits: 2, default: 0.2,
        },
        SliderSpec {
            title: tr("Space"),
            subtitle: tr("Stereo widening — 1.0 neutral, >1.0 wider. Above 1.5 hard-panned content can phase-flip; cap is intentional."),
            param: "bigspace:width",
            min: 0.0, max: 1.5, step: 0.05, digits: 2, default: 1.2,
        },
        SliderSpec {
            title: tr("Crossfeed (Headphones)"),
            subtitle: tr("Out-of-head soundstage — moves the image forward, less fatigue. 0 on speakers."),
            param: "bigcross:amount",
            min: 0.0, max: 1.0, step: 0.05, digits: 2, default: 0.3,
        },
        SliderSpec {
            title: tr("Loudness"),
            subtitle: tr("Compressor strength — higher = louder average, less dynamic"),
            param: "bigloud:amount",
            min: 0.0, max: 1.0, step: 0.05, digits: 2, default: 0.4,
        },
        SliderSpec {
            title: tr("Output Ceiling (dB)"),
            subtitle: tr("Peak limiter target — closer to 0 = louder peaks, more risk"),
            param: "bigloud:ceiling_db",
            min: -3.0, max: 0.0, step: 0.1, digits: 1, default: -1.0,
        },
    ]
}

/// Build and present the standard libadwaita About window. Contents
/// declared here travel with the binary so the dialog matches the
/// version the user is actually running, regardless of how the package
/// was assembled.
fn show_about_window(parent: &impl IsA<gtk::Window>) {
    let comments = tr(concat!(
        "System-wide audio enhancement for BigCommunity.\n\n",
        "A five-stage Rust DSP chain — BigBass (psychoacoustic bass synthesis), ",
        "BigClarity (treble exciter), BigSpace (Mid/Side widener), BigCross ",
        "(Bauer crossfeed for headphones) and BigLoud (compressor + limiter) ",
        "— routed through PipeWire's filter-chain. Auto-applies a tuned ",
        "profile when you switch output device (laptop, headphones, Bluetooth, ",
        "HDMI), with built-in presets ranging from Studio Reference to ",
        "Atmos / Cinema.",
    ));

    let about = adw::AboutWindow::builder()
        .transient_for(parent)
        .modal(true)
        .application_name("BigSound")
        .application_icon("com.bigcommunity.BigSound")
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("BigCommunity Team")
        .copyright("© 2026 BigCommunity Team")
        .license_type(gtk::License::Gpl30)
        .website("https://communitybig.org")
        .issue_url("https://github.com/xathay/bigsound/issues")
        .comments(&comments)
        .build();

    // Credit sections — the order in which we call `add_credit_section`
    // is the order they're rendered in the Credits page. Maintainer goes
    // FIRST per project preference; translator section is added last.
    about.add_credit_section(
        Some(&tr("Maintainer")),
        &["Leonardo Athayde https://github.com/xathay"],
    );
    about.add_credit_section(
        Some(&tr("DSP techniques")),
        &[
            "Aarts/Larsen — psychoacoustic bass synthesis (BigBass)",
            "Aphex / BBE — exciter topology (BigClarity)",
            "Benjamin Bauer (1961) — stereo crossfeed (BigCross)",
            "Robert Bristow-Johnson — RBJ biquad cookbook (all filters)",
            "Linkwitz-Riley — crossover phase coherence",
        ],
    );
    about.add_credit_section(
        Some(&tr("Built on")),
        &[
            "PipeWire — modern Linux audio server",
            "GTK 4 + libadwaita — Adwaita-styled UI",
            "zbus — async D-Bus for Rust",
            "Rust",
        ],
    );
    // Translator credits added as a regular section so it doesn't get
    // pinned to the top of the Credits page by libadwaita's special
    // handling of `set_translator_credits`. Empty single-line section.
    about.add_credit_section(Some(&tr("Translation")), &["Leonardo Athayde"]);

    about.present();
}

/// Profile dropdown for the header. The daemon's profile list is
/// canonical English (used as the apply-profile RPC argument); the
/// dropdown shows the localised display name via `gettext`. We keep both
/// vectors in lock-step so a user click maps cleanly back to the
/// canonical name even if a translation collides (which it shouldn't,
/// but defensive-by-default).
fn build_profile_dropdown(
    bus: &Bus,
    scales: Rc<RefCell<Vec<(gtk::Scale, SliderSpec)>>>,
    profile_group: adw::PreferencesGroup,
) -> gtk::DropDown {
    let canonical_names = bus.list_profiles();
    let display_names: Vec<String> = canonical_names.iter().map(|n| tr(n)).collect();
    let display_strs: Vec<&str> = display_names.iter().map(|s| s.as_str()).collect();
    let model = gtk::StringList::new(&display_strs);

    let dd = gtk::DropDown::builder()
        .model(&model)
        .tooltip_text(tr("Output profile (auto-applied when you change device)"))
        .build();

    let update_group_for = |bus: &Bus, group: &adw::PreferencesGroup, name: &str| {
        group.set_title(&tr(name));
        match bus.profile_description(name) {
            Some(desc) if !desc.is_empty() => group.set_description(Some(&tr(&desc))),
            _ => group.set_description(None),
        }
    };

    // Pre-select whatever the daemon reports as active. If the daemon
    // hasn't applied anything yet, fall back to "BigSound" (the OOB
    // balanced default) so the dropdown matches the cache initial state.
    let active_name = bus
        .active_profile()
        .or_else(|| Some("BigSound".to_string()));
    if let Some(active) = active_name {
        if let Some(idx) = canonical_names.iter().position(|n| n == &active) {
            dd.set_selected(idx as u32);
        }
        if let Some(name) = canonical_names.get(dd.selected() as usize) {
            update_group_for(bus, &profile_group, name);
        }
    }

    let bus = bus.clone();
    let canonical = canonical_names;
    let programmatic = Rc::new(std::cell::Cell::new(false));
    let programmatic_for_handler = programmatic.clone();
    dd.connect_selected_notify(move |dd: &gtk::DropDown| {
        if programmatic_for_handler.get() {
            return;
        }
        let idx = dd.selected() as usize;
        let Some(name) = canonical.get(idx) else {
            return;
        };
        bus.apply_profile(name);
        update_group_for(&bus, &profile_group, name);
        for (scale, spec) in scales.borrow().iter() {
            if let Some(v) = bus.get(spec.param) {
                programmatic_for_handler.set(true);
                scale.set_value(v);
                programmatic_for_handler.set(false);
            }
        }
    });

    dd
}

/// Build one slider row. Reads the current value from the daemon so the
/// UI matches reality on launch, and writes back on `value-changed`.
fn build_slider_row(
    spec: &SliderSpec,
    bus: &Bus,
    scales: &Rc<RefCell<Vec<(gtk::Scale, SliderSpec)>>>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&spec.title)
        .subtitle(&spec.subtitle)
        .build();

    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, spec.min, spec.max, spec.step);
    scale.set_size_request(280, -1);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Left);
    scale.set_digits(spec.digits);
    scale.set_hexpand(true);
    scale.set_valign(gtk::Align::Center);

    // Initial value from the daemon — fall back to default if read fails.
    let current = bus.get(spec.param).unwrap_or(spec.default);
    scale.set_value(current);

    {
        let bus = bus.clone();
        let param = spec.param.to_string();
        scale.connect_value_changed(move |s| {
            bus.set(&param, s.value());
        });
    }

    scales.borrow_mut().push((scale.clone(), spec.clone()));
    row.add_suffix(&scale);
    row
}

/// Build the "Output device" combo row. The first entry is always
/// "Automatic" (follow WirePlumber priority — the out-of-box behaviour);
/// the rest are the real sinks the daemon reports. Picking a specific
/// device pins BigSound's DSP output there so a high-priority USB gadget
/// (e.g. a microphone that also exposes a playback endpoint) can't steal
/// the routing. The device list is read once at window build; reopening
/// the window refreshes it.
fn build_output_device_row(bus: &Bus) -> adw::ComboRow {
    let devices = bus.list_output_devices();
    let current = bus.output_device();

    // Index 0 = automatic (empty node.name); the rest mirror the daemon's
    // order. `names` and the model rows stay in lock-step so a selection
    // maps cleanly back to the canonical node.name.
    let mut names: Vec<String> = vec![String::new()];
    let mut labels: Vec<String> = vec![tr("Automatic (follow priority)")];
    for (name, desc) in &devices {
        names.push(name.clone());
        labels.push(desc.clone());
    }
    let label_strs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let model = gtk::StringList::new(&label_strs);

    let row = adw::ComboRow::builder()
        .title(tr("Output device"))
        .subtitle(tr("Which speakers or headphones BigSound plays through"))
        .build();
    row.set_model(Some(&model));

    // Pre-select the daemon's current choice before wiring the handler so
    // the initial set_selected doesn't fire a redundant SetOutputDevice.
    let sel = names.iter().position(|n| n == &current).unwrap_or(0);
    row.set_selected(sel as u32);

    let bus = bus.clone();
    row.connect_selected_notify(move |r| {
        if let Some(name) = names.get(r.selected() as usize) {
            bus.set_output_device(name);
        }
    });

    row
}

fn build_window(app: &adw::Application, bus: Bus) {
    let scales: Rc<RefCell<Vec<(gtk::Scale, SliderSpec)>>> = Rc::new(RefCell::new(Vec::new()));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("BigSound")
        .icon_name("com.bigcommunity.BigSound")
        .default_width(560)
        .default_height(680)
        .build();

    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();

    // Reset button in the header.
    let reset_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
    reset_btn.set_tooltip_text(Some(&tr("Reset to defaults")));
    {
        let scales = scales.clone();
        let bus = bus.clone();
        reset_btn.connect_clicked(move |_| {
            for (scale, spec) in scales.borrow().iter() {
                scale.set_value(spec.default);
                bus.set(spec.param, spec.default);
            }
        });
    }
    header.pack_start(&reset_btn);

    // About button — opens the standard libadwaita About window.
    let about_btn = gtk::Button::from_icon_name("help-about-symbolic");
    about_btn.set_tooltip_text(Some(&tr("About BigSound")));
    {
        let window_weak = window.downgrade();
        about_btn.connect_clicked(move |_| {
            if let Some(parent) = window_weak.upgrade() {
                show_about_window(&parent);
            }
        });
    }
    header.pack_start(&about_btn);

    main_box.append(&header);

    let page = adw::PreferencesPage::new();
    page.set_vexpand(true);

    // Single sliders group — its title and description are repurposed
    // as the *active profile* indicator so the user sees what preset
    // is loaded without an extra block above. The dropdown handler
    // refreshes both fields whenever the selection changes.
    let group = adw::PreferencesGroup::new();

    // Profile dropdown on the right side of the header. The dropdown
    // owns the group so its selection-changed handler can refresh the
    // title + description in place.
    let profile_dd = build_profile_dropdown(&bus, scales.clone(), group.clone());
    header.pack_end(&profile_dd);

    for spec in slider_specs() {
        group.add(&build_slider_row(&spec, &bus, &scales));
    }

    page.add(&group);

    // Routing group: choose the real device BigSound plays through, with
    // a reminder to select BigSound as the system output sink.
    let routing_group = adw::PreferencesGroup::new();
    routing_group.set_description(Some(&tr(
        "Pick \"BigSound (DSP)\" in Settings → Sound → Output to route audio through BigSound.",
    )));
    routing_group.add(&build_output_device_row(&bus));
    page.add(&routing_group);

    main_box.append(&page);

    window.set_content(Some(&main_box));
    window.present();
}

fn main() -> glib::ExitCode {
    init_i18n();

    let bus = match Bus::connect() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{}: {e}", tr("BigSound app: cannot reach session bus"));
            return glib::ExitCode::FAILURE;
        }
    };

    // Probe the daemon up front — bail early with a useful message if it
    // isn't running, instead of leaving the user with a window full of
    // sliders that don't do anything.
    if bus
        .proxy()
        .and_then(|p| p.call::<_, _, Vec<String>>("List", &()))
        .is_err()
    {
        eprintln!(
            "{}\n{}",
            tr("BigSound app: bigsound-daemon doesn't seem to be running."),
            tr("Try: systemctl --user start bigsound-daemon.service"),
        );
        return glib::ExitCode::FAILURE;
    }

    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| build_window(app, bus.clone()));
    app.run()
}
