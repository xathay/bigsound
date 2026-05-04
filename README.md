<p align="center">
  <img src="assets/icon.png" alt="BigSound" width="128" height="128"/>
</p>

<h1 align="center">BigSound</h1>

<p align="center">
  System-wide audio enhancement for <b>BigCommunity</b> — a five-stage Rust DSP
  chain routed through PipeWire's filter-chain, with auto-profile per
  output device and a libadwaita frontend for live tuning.
</p>

---

Plays nicely with whatever's already running: Firefox, Spotify, MPV,
Discord, games. Pick "BigSound (DSP)" as your audio output and every app
on the system goes through the chain.

## What it does

| Module        | Role                                                        |
|---------------|-------------------------------------------------------------|
| **BigBass**   | Psychoacoustic bass enhancement (missing-fundamental synth) |
| **BigClarity**| Treble exciter — sparkle on hi-hats, vocals, strings        |
| **BigSpace**  | Stereo widening (Mid/Side) with bass-keep-mono safety       |
| **BigCross**  | Bauer crossfeed — out-of-head soundstage on headphones      |
| **BigLoud**   | Stereo-linked compressor + limiter, calibrated make-up gain |

The chain runs at native sample rate inside PipeWire's realtime audio
path. No FFT, no oversampling, no allocations during process — just
biquads and waveshapers, sample-accurate.

## Auto-profile

Plug headphones, switch to Bluetooth, change to HDMI — BigSound
detects the active output (sink + port via PipeWire) and applies a
matching profile automatically. No clicking, no fiddling.

Built-in profiles:

- **Laptop Speaker** — aggressive bass + heavy compression for tiny
  rolled-off speakers
- **Headphones** — gentle, dynamics preserved, crossfeed on
- **Bluetooth** — bandwidth-aware tuning for BT codec losses
- **HDMI / TV** — neutral, conservative defaults
- **BigSound** (fallback) — balanced hi-fi-friendly default that works
  on any unknown hardware

Plus 9 hand-crafted manual presets: *Studio Reference*, *Atmos / Cinema*,
*Audiophile*, *Punchy*, *Gaming*, *Voice / Podcast*, *Bass Heavy*,
*Late Night*, *Live / Concert*.

## Install

### Arch / Manjaro / BigCommunity (recommended)

```bash
git clone https://github.com/xathay/bigsound.git
cd bigsound/packaging
makepkg -si
systemctl --user enable --now filter-chain.service bigsound-daemon.service
```

Then open *Settings → Sound → Output* and pick **BigSound (DSP)**.

### From source (no root needed)

```bash
git clone https://github.com/xathay/bigsound.git
cd bigsound
./scripts/install.sh
```

The script builds in release mode, installs LADSPA plugins to
`~/.ladspa/`, drops the PipeWire filter-chain config in
`~/.config/pipewire/filter-chain.conf.d/`, and starts the daemon.
Requires `pipewire >= 1.0`, `gtk4`, `libadwaita`, `gettext`, and a Rust
toolchain.

## Use

After installing, **BigSound** appears in the GNOME app launcher.
Open it for live sliders + profile dropdown. From the terminal:

```bash
bigsound show                       # snapshot of current parameters
bigsound profile list               # all profiles
bigsound profile apply Audiophile   # switch profile manually
bigsound set bigloud:amount 0.7     # tune one parameter live
bigsound profile save MyMix         # save current state as a new profile
```

## Architecture

```
apps  ──► BigSound (PipeWire sink)
              │
              └── filter-chain
                    ├── bigbass_l/r       (mono)
                    ├── bigclarity_l/r    (mono)
                    ├── bigspace          (stereo)
                    ├── bigcross          (stereo)
                    └── bigloud           (stereo)
              │
              ▼
       default real output (analog / HDMI / BT / USB)

D-Bus ◄──── bigsound-daemon ────► pw-cli set-param (live tuning)
GUI/CLI                              (no service restart)
```

Each DSP module is a separate Rust crate compiled to a LADSPA `.so`;
the PipeWire `module-filter-chain` strings them together. The Rust
daemon (`com.bigcommunity.BigSound1`) caches parameters in memory,
pushes to PipeWire, and watches for output-device changes to auto-apply
the matching profile.

## Internationalisation

Strings are wrapped with `gettext`. Source language is English (the
`.po` template), Brazilian Portuguese is shipped (`crates/gtk-app/po/pt_BR.po`).
To add another language, copy `pt_BR.po`, translate the `msgstr`
entries, name it `<locale>.po` and reinstall.

## License

GPL-3.0-or-later. See [PKGBUILD](packaging/PKGBUILD) and the LICENSE
generated at install time.

## Credits

Built by the **BigCommunity Team**. Maintainer:
[Leonardo Athayde](https://github.com/xathay).

DSP techniques credited to: Aarts/Larsen (psychoacoustic bass),
Aphex / BBE (exciter topology), Benjamin Bauer (1961, stereo crossfeed),
Robert Bristow-Johnson (RBJ biquad cookbook), Linkwitz-Riley (crossover
phase coherence).

Built on PipeWire, GTK 4, libadwaita, zbus, Rust.
