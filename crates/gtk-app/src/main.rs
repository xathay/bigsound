//! BigSound GTK4 + libadwaita frontend.
//!
//! Main and advanced views for the five-stage audio chain. A bounded
//! worker serializes and coalesces writes, keeping blocking D-Bus off GTK.

use gettextrs::{bind_textdomain_codeset, bindtextdomain, gettext, textdomain};
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

mod worker;
use std::rc::Rc;
use worker::Command;
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
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|path| std::path::Path::new(path).is_absolute())
        .unwrap_or_else(|| format!("{home}/.local/share"));
    let user_dir = format!("{data_home}/locale");
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

    fn set(&self, param: &str, value: f64) -> zbus::Result<()> {
        self.proxy()?.call("Set", &(param, value))
    }

    fn list_profiles(&self) -> Vec<String> {
        self.proxy()
            .and_then(|p| p.call("ListProfiles", &()))
            .unwrap_or_default()
    }

    fn apply_profile(&self, name: &str) -> zbus::Result<()> {
        self.proxy()?.call("ApplyProfile", &(name,))
    }

    /// Route BigSound's DSP output to `name` (a sink `node.name`), or pass
    /// "" to return to automatic priority-based routing.
    fn set_output_device(&self, name: &str) -> zbus::Result<()> {
        self.proxy()?.call("SetOutputDevice", &(name,))
    }
}

/// Show failed writes in the UI instead of presenting them as successful.
fn show_bus_error(widget: &impl IsA<gtk::Widget>, error: &str) {
    if let Some(window) = widget
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    {
        let dialog =
            adw::MessageDialog::new(Some(&window), Some(&tr("Audio update failed")), Some(error));
        dialog.add_response("close", &tr("Close"));
        dialog.set_close_response("close");
        dialog.present();
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

const BASIC_PARAMS: &[&str] = &[
    "bigbass:loudness_db",
    "bigclarity:mix",
    "bigspace:width",
    "bigloud:amount",
];

fn slider_specs() -> Vec<SliderSpec> {
    let spec = |title: &str, subtitle: &str, param, min, max, step, digits, default| SliderSpec {
        title: tr(title),
        subtitle: tr(subtitle),
        param,
        min,
        max,
        step,
        digits,
        default,
    };
    vec![
        spec(
            "Bass",
            "Bass enhancement make-up gain, in dB",
            "bigbass:loudness_db",
            -12.0,
            12.0,
            0.5,
            1,
            2.5,
        ),
        spec(
            "Bass Frequency (Hz)",
            "Frequency used to synthesise bass harmonics",
            "bigbass:target_freq",
            40.0,
            300.0,
            5.0,
            0,
            90.0,
        ),
        spec(
            "Bass Drive",
            "Nonlinear harmonic generation strength",
            "bigbass:drive",
            0.0,
            1.0,
            0.05,
            2,
            0.45,
        ),
        spec(
            "Bass Mix",
            "Amount of synthesised bass added to the original",
            "bigbass:mix",
            0.0,
            1.0,
            0.05,
            2,
            0.35,
        ),
        spec(
            "Filter Original Bass",
            "Remove lows from the original signal before adding harmonics",
            "bigbass:cut_dry_lows",
            0.0,
            1.0,
            1.0,
            0,
            0.0,
        ),
        spec(
            "Clarity",
            "Amount of treble enhancement",
            "bigclarity:mix",
            0.0,
            1.0,
            0.05,
            2,
            0.2,
        ),
        spec(
            "Clarity Frequency (Hz)",
            "Frequency region used by the treble exciter",
            "bigclarity:target_freq",
            1500.0,
            8000.0,
            100.0,
            0,
            4000.0,
        ),
        spec(
            "Clarity Drive",
            "Treble harmonic generation strength",
            "bigclarity:drive",
            0.0,
            1.0,
            0.05,
            2,
            0.3,
        ),
        spec(
            "Space",
            "1.0 is neutral; above 1.5 can alter phase in hard-panned audio",
            "bigspace:width",
            0.0,
            2.0,
            0.05,
            2,
            1.2,
        ),
        spec(
            "Bass Centre (Hz)",
            "Keep low frequencies centred in the stereo image",
            "bigspace:bass_keep_hz",
            40.0,
            400.0,
            10.0,
            0,
            150.0,
        ),
        spec(
            "Space Mix",
            "Amount of stereo processing mixed with the original",
            "bigspace:mix",
            0.0,
            1.0,
            0.05,
            2,
            1.0,
        ),
        spec(
            "Crossfeed (Headphones)",
            "Filtered channel blending for headphones; use zero on speakers",
            "bigcross:amount",
            0.0,
            1.0,
            0.05,
            2,
            0.3,
        ),
        spec(
            "Crossfeed Cutoff (Hz)",
            "Low-pass cutoff of the signal blended between channels",
            "bigcross:cutoff_hz",
            400.0,
            1500.0,
            50.0,
            0,
            700.0,
        ),
        spec(
            "Crossfeed Delay (µs)",
            "Delay of the signal blended between channels",
            "bigcross:delay_us",
            100.0,
            500.0,
            10.0,
            0,
            280.0,
        ),
        spec(
            "Compression",
            "Higher values reduce dynamics and raise average loudness",
            "bigloud:amount",
            0.0,
            1.0,
            0.05,
            2,
            0.4,
        ),
        spec(
            "Output Ceiling (dB)",
            "Sample-peak ceiling; does not measure inter-sample true peaks",
            "bigloud:ceiling_db",
            -12.0,
            0.0,
            0.1,
            1,
            -1.0,
        ),
        spec(
            "Compression Mix",
            "Amount of compressed audio mixed with the original",
            "bigloud:mix",
            0.0,
            1.0,
            0.05,
            2,
            1.0,
        ),
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
        .issue_url("https://github.com/big-comm/bigsound/issues")
        .comments(&comments)
        .build();

    // Credit sections appear in insertion order; translation is listed last.
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
    about.add_credit_section(Some(&tr("Translation")), &["BigCommunity Team"]);

    about.present();
}

#[derive(Clone)]
struct Snapshot {
    profile: String,
    parameters: BTreeMap<String, f64>,
    devices: Vec<(String, String)>,
    output: String,
}

impl Bus {
    fn snapshot(&self) -> zbus::Result<Snapshot> {
        let proxy = self.proxy()?;
        let profile: String = proxy.get_property("ActiveProfile")?;
        let mut parameters = BTreeMap::new();
        for spec in slider_specs() {
            parameters.insert(spec.param.to_string(), proxy.call("Get", &(spec.param,))?);
        }
        Ok(Snapshot {
            profile: if profile.is_empty() {
                "BigSound".into()
            } else {
                profile
            },
            parameters,
            devices: proxy.call("ListOutputDevices", &())?,
            output: proxy.call("GetOutputDevice", &())?,
        })
    }
}

#[derive(Clone)]
struct Dispatcher {
    sender: SyncSender<Command>,
    pending: Rc<Cell<usize>>,
}

impl Dispatcher {
    fn send(&self, widget: &impl IsA<gtk::Widget>, command: Command) {
        match self.sender.try_send(command) {
            Ok(()) => self.pending.set(self.pending.get() + 1),
            Err(_) => show_bus_error(widget, &tr("Audio controls are busy. Please try again.")),
        }
    }
}

#[derive(Clone)]
enum Control {
    Scale(gtk::Scale),
    Switch(gtk::Switch),
}

type Controls = Rc<RefCell<Vec<(Control, SliderSpec)>>>;

fn build_control_row(
    spec: &SliderSpec,
    dispatch: &Dispatcher,
    controls: &Controls,
    programmatic: &Rc<Cell<bool>>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&spec.title)
        .subtitle(&spec.subtitle)
        .build();
    row.set_title_lines(1);
    row.set_subtitle_lines(2);
    if spec.param == "bigbass:cut_dry_lows" {
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        switch.set_tooltip_text(Some(&spec.title));
        switch.update_property(&[gtk::accessible::Property::Label(&spec.title)]);
        let dispatcher = dispatch.clone();
        let updating = programmatic.clone();
        switch.connect_active_notify(move |switch| {
            if !updating.get() {
                dispatcher.send(
                    switch,
                    Command::Set(
                        "bigbass:cut_dry_lows",
                        if switch.is_active() { 1.0 } else { 0.0 },
                    ),
                );
            }
        });
        controls
            .borrow_mut()
            .push((Control::Switch(switch.clone()), spec.clone()));
        row.add_suffix(&switch);
        row.set_activatable_widget(Some(&switch));
    } else {
        let scale =
            gtk::Scale::with_range(gtk::Orientation::Horizontal, spec.min, spec.max, spec.step);
        scale.set_size_request(220, -1);
        scale.set_draw_value(true);
        scale.set_value_pos(gtk::PositionType::Left);
        scale.set_digits(spec.digits);
        scale.set_valign(gtk::Align::Center);
        scale.set_tooltip_text(Some(&spec.title));
        scale.update_property(&[gtk::accessible::Property::Label(&spec.title)]);
        scale.set_value(spec.default);
        if spec.min == 0.0 && spec.max == 1.0 {
            scale.set_format_value_func(|_, value| format!("{:.0}%", value * 100.0));
        } else if spec.param == "bigspace:width" {
            scale.set_format_value_func(|_, value| format!("{}×", factor_text(value)));
        }
        let dispatcher = dispatch.clone();
        let updating = programmatic.clone();
        let param = spec.param;
        scale.connect_value_changed(move |scale| {
            if !updating.get() {
                dispatcher.send(scale, Command::Set(param, scale.value()));
            }
        });
        controls
            .borrow_mut()
            .push((Control::Scale(scale.clone()), spec.clone()));
        row.add_suffix(&scale);
    }
    row
}

fn profile_summary(name: &str) -> String {
    tr(match name {
        "BigSound" => "Balanced enhancement for everyday listening.",
        "Studio Reference" => "Minimal processing for reference listening.",
        "Audiophile" => "Restrained enhancement and gentle headphone crossfeed.",
        "Atmos / Cinema" | "Headphones - Cinema" => {
            "Stereo cinema preset with bass and spatial processing."
        }
        "Gaming" => "Stereo processing for games; compare positional cues.",
        "Voice / Podcast" => "Vocal clarity and compression for dialogue.",
        "Bass Heavy" => "Strong bass harmonics and compression.",
        "Late Night" => "Reduced bass and steadier dialogue for quiet listening.",
        "Live / Concert" => "Wider stereo and gentle enhancement for live recordings.",
        "Punchy" | "Laptop Speaker" => "Strong enhancement for small speakers.",
        "Headphones" => "Gentle enhancement with headphone crossfeed.",
        "Bluetooth" => "Bass and treble enhancement for Bluetooth outputs.",
        "HDMI / TV" => "Conservative settings for TV and display speakers.",
        _ => "Custom listening profile.",
    })
}

fn profile_icon(name: &str) -> &'static str {
    match name {
        "Headphones" | "Headphones - Cinema" | "Audiophile" => "audio-headphones-symbolic",
        "Bluetooth" => "bluetooth-active-symbolic",
        "HDMI / TV" | "Atmos / Cinema" => "video-display-symbolic",
        "Gaming" => "applications-games-symbolic",
        "Voice / Podcast" => "audio-input-microphone-symbolic",
        _ => "audio-speakers-symbolic",
    }
}

const SUMMARY: &[(&str, &str, f64)] = &[
    ("Bass Mix", "bigbass:mix", 1.0),
    ("Clarity", "bigclarity:mix", 1.0),
    ("Space", "bigspace:width", 2.0),
    ("Crossfeed", "bigcross:amount", 1.0),
    ("Compression", "bigloud:amount", 1.0),
];

fn fraction(value: f64, maximum: f64) -> f64 {
    if value.is_finite() && maximum.is_finite() && maximum > 0.0 {
        (value / maximum).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn factor_text(value: f64) -> String {
    format!("{value:.2}").replace('.', &gettextrs::pgettext("decimal separator", "."))
}

fn build_window(app: &adw::Application, bus: Bus) {
    let initial = match bus.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("{}: {error}", tr("Audio update failed"));
            return;
        }
    };
    let names = Rc::new(bus.list_profiles());
    let state = Rc::new(RefCell::new(initial));
    let updating = Rc::new(Cell::new(false));
    let controls: Controls = Rc::new(RefCell::new(Vec::new()));
    let (sender, events) = worker::start(bus);
    let dispatch = Dispatcher {
        sender,
        pending: Rc::new(Cell::new(0)),
    };
    let closing = Rc::new(Cell::new(false));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("BigSound")
        .icon_name(APP_ID)
        .default_width(760)
        .default_height(780)
        .build();
    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    {
        let pending = dispatch.pending.clone();
        let closing = closing.clone();
        window.connect_close_request(move |window| {
            if pending.get() > 0 {
                closing.set(true);
                window.set_cursor_from_name(Some("wait"));
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    let stack = adw::ViewStack::new();
    stack.set_vexpand(true);
    for (name, view, accelerator) in [
        ("main-view", "main", "<Primary>1"),
        ("advanced-view", "advanced", "<Primary>2"),
    ] {
        let action = gtk::gio::SimpleAction::new(name, None);
        let weak = stack.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(stack) = weak.upgrade() {
                stack.set_visible_child_name(view);
            }
        });
        app.add_action(&action);
        app.set_accels_for_action(&format!("app.{name}"), &[accelerator]);
    }
    let header = adw::HeaderBar::new();
    let icon = gtk::Image::from_icon_name(APP_ID);
    icon.set_pixel_size(28);
    icon.set_tooltip_text(Some("BigSound"));
    header.pack_start(&icon);
    let reset = gtk::Button::from_icon_name("view-refresh-symbolic");
    reset.set_tooltip_text(Some(&tr("Restore selected profile")));
    {
        let dispatcher = dispatch.clone();
        let state = state.clone();
        reset.connect_clicked(move |button| {
            dispatcher.send(button, Command::Profile(state.borrow().profile.clone()))
        });
    }
    header.pack_start(&reset);
    let about = gtk::Button::from_icon_name("help-about-symbolic");
    about.set_tooltip_text(Some(&tr("About BigSound")));
    let weak = window.downgrade();
    about.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            show_about_window(&window);
        }
    });
    header.pack_end(&about);
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    header.set_title_widget(Some(&switcher));
    main_box.append(&header);

    let main = adw::PreferencesPage::new();
    let profiles = adw::PreferencesGroup::builder()
        .title(tr("Listening profile"))
        .build();
    let row = adw::ActionRow::builder().title(tr("Profile")).build();
    row.set_subtitle_lines(2);
    let profile_image = gtk::Image::from_icon_name("audio-speakers-symbolic");
    row.add_prefix(&profile_image);
    let display: Vec<String> = names.iter().map(|s| tr(s)).collect();
    let display_refs: Vec<&str> = display.iter().map(String::as_str).collect();
    let dropdown = gtk::DropDown::from_strings(&display_refs);
    dropdown.set_valign(gtk::Align::Center);
    row.add_suffix(&dropdown);
    row.set_activatable_widget(Some(&dropdown));
    {
        let dispatcher = dispatch.clone();
        let updating = updating.clone();
        let names = names.clone();
        dropdown.connect_selected_notify(move |dd| {
            if !updating.get() {
                if let Some(name) = names.get(dd.selected() as usize) {
                    dispatcher.send(dd, Command::Profile(name.clone()));
                }
            }
        });
    }
    profiles.add(&row);
    main.add(&profiles);

    let routing = adw::PreferencesGroup::new();
    routing.set_description(Some(&tr(
        "Select BigSound (DSP) as the system output in Sound settings.",
    )));
    let output = adw::ComboRow::builder()
        .title(tr("Output device"))
        .subtitle(tr("Which speakers or headphones BigSound plays through"))
        .build();
    let devices = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    {
        let dispatcher = dispatch.clone();
        let updating = updating.clone();
        let devices = devices.clone();
        output.connect_selected_notify(move |row| {
            if updating.get() {
                return;
            }
            let name = if row.selected() == 0 {
                String::new()
            } else {
                let Some(name) = devices
                    .borrow()
                    .get(row.selected() as usize - 1)
                    .map(|v| v.0.clone())
                else {
                    return;
                };
                name
            };
            dispatcher.send(row, Command::Output(name));
        });
    }
    routing.add(&output);
    main.add(&routing);

    let summary_group = adw::PreferencesGroup::builder()
        .title(tr("Control overview"))
        .description(tr(
            "Current control values; this does not measure the playing audio.",
        ))
        .build();
    let summary_box = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    summary_box.set_homogeneous(true);
    summary_box.set_margin_top(8);
    summary_box.set_margin_bottom(8);
    let mut bars = Vec::new();
    for (title, _, _) in SUMMARY {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 7);
        let label = gtk::Label::new(Some(&tr(title)));
        label.add_css_class("caption");
        let value = gtk::Label::new(None);
        value.add_css_class("heading");
        let bar = gtk::ProgressBar::new();
        bar.update_property(&[gtk::accessible::Property::Label(&tr(title))]);
        column.append(&label);
        column.append(&value);
        column.append(&bar);
        summary_box.append(&column);
        bars.push((value, bar));
    }
    summary_group.add(&summary_box);
    main.add(&summary_group);

    let basic = adw::PreferencesGroup::builder()
        .title(tr("Quick adjustments"))
        .build();
    let specs = slider_specs();
    for spec in specs.iter().filter(|s| BASIC_PARAMS.contains(&s.param)) {
        let mut basic_spec = spec.clone();
        if spec.param == "bigspace:width" {
            basic_spec.subtitle =
                tr("Stereo width; 1.0 keeps the original image. Increase carefully.");
        }
        basic.add(&build_control_row(
            &basic_spec,
            &dispatch,
            &controls,
            &updating,
        ));
    }
    main.add(&basic);
    stack.add_titled_with_icon(&main, Some("main"), &tr("Main"), "audio-speakers-symbolic");

    let advanced = adw::PreferencesPage::new();
    let context = adw::PreferencesGroup::builder()
        .title(tr("Advanced settings"))
        .build();
    advanced.add(&context);
    for (prefix, title) in [
        ("bigbass:", "Bass synthesis"),
        ("bigclarity:", "Treble exciter"),
        ("bigspace:", "Stereo image"),
        ("bigcross:", "Headphone crossfeed"),
        ("bigloud:", "Dynamics and output"),
    ] {
        let group = adw::PreferencesGroup::builder().title(tr(title)).build();
        for spec in specs.iter().filter(|s| s.param.starts_with(prefix)) {
            group.add(&build_control_row(spec, &dispatch, &controls, &updating));
        }
        advanced.add(&group);
    }
    stack.add_titled_with_icon(
        &advanced,
        Some("advanced"),
        &tr("Advanced"),
        "preferences-system-symbolic",
    );
    stack.set_visible_child_name("main");
    main_box.append(&stack);
    window.set_content(Some(&main_box));

    let sync: Rc<dyn Fn(&Snapshot)> = Rc::new({
        let updating = updating.clone();
        let controls = controls.clone();
        let names = names.clone();
        let output = output.clone();
        let devices = devices.clone();
        let dropdown = dropdown.clone();
        move |snapshot| {
            updating.set(true);
            row.set_subtitle(&profile_summary(&snapshot.profile));
            profile_image.set_icon_name(Some(profile_icon(&snapshot.profile)));
            if let Some(index) = names.iter().position(|name| name == &snapshot.profile) {
                row.set_title(&tr("Profile"));
                dropdown.set_selected(index as u32);
            } else {
                row.set_use_markup(false);
                row.set_title(&snapshot.profile);
                dropdown.set_selected(gtk::INVALID_LIST_POSITION);
            }
            let description = tr("Active profile: {profile}. Changes are applied live.")
                .replace("{profile}", &tr(&snapshot.profile));
            context.set_description(Some(&description));
            if *devices.borrow() != snapshot.devices || output.model().is_none() {
                let mut labels = vec![tr("Automatic")];
                labels.extend(snapshot.devices.iter().map(|v| v.1.clone()));
                let references: Vec<&str> = labels.iter().map(String::as_str).collect();
                output.set_model(Some(&gtk::StringList::new(&references)));
                *devices.borrow_mut() = snapshot.devices.clone();
            }
            let selected = snapshot
                .devices
                .iter()
                .position(|v| v.0 == snapshot.output)
                .map(|i| i + 1)
                .unwrap_or(0);
            output.set_selected(selected as u32);
            for (widget, spec) in controls.borrow().iter() {
                if let Some(value) = snapshot.parameters.get(spec.param) {
                    match widget {
                        Control::Scale(scale) => scale.set_value(*value),
                        Control::Switch(switch) => switch.set_active(*value > 0.5),
                    }
                }
            }
            for ((title, param, maximum), (label, bar)) in SUMMARY.iter().zip(bars.iter()) {
                if let Some(value) = snapshot.parameters.get(*param) {
                    let formatted = if *param == "bigspace:width" {
                        format!("{}×", factor_text(*value))
                    } else {
                        format!("{:.0}%", value * 100.0)
                    };
                    label.set_text(&formatted);
                    bar.set_fraction(fraction(*value, *maximum));
                    bar.set_tooltip_text(Some(&format!("{}: {}", tr(title), formatted)));
                }
            }
            updating.set(false);
        }
    });
    sync(&state.borrow());
    let weak = window.downgrade();
    let mut disconnected = false;
    glib::timeout_add_local(Duration::from_millis(40), move || {
        let Some(window) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        for event in events.try_iter() {
            dispatch
                .pending
                .set(dispatch.pending.get().saturating_sub(event.completed));
            if let Some(error) = event.error {
                closing.set(false);
                window.set_cursor_from_name(None);
                if event.completed > 0 || !disconnected {
                    show_bus_error(&window, &error);
                }
                disconnected = true;
            } else {
                disconnected = false;
            }
            if let Some(snapshot) = event.snapshot {
                if dispatch.pending.get() == 0 {
                    sync(&snapshot);
                    *state.borrow_mut() = snapshot;
                }
            } else if event.completed > 0 && dispatch.pending.get() == 0 {
                // A disconnected daemon cannot supply a fresh snapshot.
                // Restore the last confirmed state instead of keeping an
                // optimistic slider value that was never accepted.
                sync(&state.borrow());
            }
        }
        if closing.get() && dispatch.pending.get() == 0 {
            window.set_cursor_from_name(None);
            window.close();
        }
        glib::ControlFlow::Continue
    });
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

    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
        } else {
            build_window(app, bus.clone());
        }
    });
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_bundled_profiles_fit_the_seventeen_exposed_controls() {
        let specs = slider_specs();
        assert_eq!(specs.len(), 17);
        let names: std::collections::BTreeSet<_> = specs.iter().map(|s| s.param).collect();
        assert_eq!(names.len(), 17);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../daemon/data/profiles");
        let mut profiles = 0;
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let profile: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let params = profile["params"].as_object().unwrap();
            assert_eq!(params.len(), 17);
            for spec in &specs {
                let value = params[spec.param].as_f64().unwrap();
                assert!(
                    value.is_finite() && value >= spec.min && value <= spec.max,
                    "{} {}",
                    profile["name"],
                    spec.param
                );
            }
            profiles += 1;
        }
        assert_eq!(profiles, 15);
    }

    #[test]
    fn overview_uses_control_units_and_rejects_invalid_numbers() {
        assert_eq!(fraction(0.2, 1.0), 0.2);
        assert_eq!(fraction(1.2, 2.0), 0.6);
        assert_eq!(fraction(-1.0, 1.0), 0.0);
        assert_eq!(fraction(4.0, 2.0), 1.0);
        assert_eq!(fraction(f64::NAN, 1.0), 0.0);
        assert_eq!(fraction(f64::INFINITY, 1.0), 0.0);
        assert_eq!(fraction(1.0, 0.0), 0.0);
    }
}
