//! Offline CLI — runs BigBass over a WAV file. Use it for A/B comparison
//! during DSP tuning, before any realtime plumbing exists.

use anyhow::{Context, bail};
use big_bass::{BassEnhancer, BassEnhancerParams};
use clap::Parser;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "big-bass-cli",
    about = "BigBass — psychoacoustic bass enhancement (offline WAV processor)"
)]
struct Cli {
    /// Input WAV file
    #[arg(short, long)]
    input: PathBuf,

    /// Output WAV file (will be written as 32-bit float)
    #[arg(short, long)]
    output: PathBuf,

    /// Lower edge of the band the speaker can't reproduce, in Hz.
    /// Laptop speaker ≈ 150-180. Bookshelf ≈ 80-100.
    #[arg(long, default_value_t = 100.0)]
    target: f32,

    /// Waveshaper drive (0..=100) — harmonic richness
    #[arg(long, default_value_t = 60.0)]
    drive: f32,

    /// Mix amount of synthesised harmonics (0..=100)
    #[arg(long, default_value_t = 50.0)]
    mix: f32,

    /// High-pass the dry signal at `--target` to remove un-reproducible lows
    #[arg(long)]
    cut_dry_lows: bool,

    /// Make-up gain in dB (-12..=+12). Positive values raise perceived
    /// loudness; the built-in peak limiter prevents clipping.
    #[arg(long, default_value_t = 4.0)]
    loudness: f32,

    /// Bypass — copy input straight through (sanity check)
    #[arg(long)]
    bypass: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if !(0.0..=100.0).contains(&cli.drive) {
        bail!("--drive must be in 0..=100");
    }
    if !(0.0..=100.0).contains(&cli.mix) {
        bail!("--mix must be in 0..=100");
    }
    if !(20.0..=500.0).contains(&cli.target) {
        bail!("--target must be in 20..=500 Hz");
    }
    if !(-12.0..=12.0).contains(&cli.loudness) {
        bail!("--loudness must be in -12..=+12 dB");
    }

    let mut reader = WavReader::open(&cli.input)
        .with_context(|| format!("opening {}", cli.input.display()))?;
    let spec = reader.spec();
    let n_ch = spec.channels as usize;
    let sr = spec.sample_rate as f32;

    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .context("reading float samples")?,
        (SampleFormat::Int, bps) if bps <= 32 => {
            let scale = 1.0 / ((1i64 << (bps - 1)) as f32);
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .context("reading int samples")?
        }
        (fmt, bps) => bail!("unsupported WAV format: {:?} @ {} bits", fmt, bps),
    };

    let params = BassEnhancerParams {
        target_freq: cli.target,
        drive: cli.drive / 100.0,
        mix: cli.mix / 100.0,
        cut_dry_lows: cli.cut_dry_lows,
        loudness_db: cli.loudness,
        bypass: cli.bypass,
    };

    let mut buf = samples;
    let mut enhancer = BassEnhancer::new(n_ch, sr, params);
    enhancer.process_interleaved(&mut buf);

    // Soft-knee limiter — only touches samples that would otherwise clip.
    // Below -1.4 dBFS (|x| ≤ 0.85) the signal passes untouched, so the
    // unprocessed bulk of the track does not pick up extra distortion.
    // Above the knee, a tanh-shaped curve compresses smoothly into the
    // [-1, 1] ceiling.
    if !params.bypass {
        const THRESHOLD: f32 = 0.85;
        const HEADROOM: f32 = 1.0 - THRESHOLD;
        for s in &mut buf {
            let abs = s.abs();
            if abs > THRESHOLD {
                let over = (abs - THRESHOLD) / HEADROOM;
                *s = s.signum() * (THRESHOLD + HEADROOM * over.tanh());
            }
        }
    }

    let out_spec = WavSpec {
        channels: spec.channels,
        sample_rate: spec.sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::create(&cli.output, out_spec)
        .with_context(|| format!("creating {}", cli.output.display()))?;
    for s in &buf {
        writer.write_sample(*s)?;
    }
    writer.finalize()?;

    println!(
        "wrote {} frames ({} ch @ {:.0} Hz) → {}\n  target={:.0} Hz · drive={:.0}% · mix={:.0}% · cut_dry={} · bypass={}",
        buf.len() / n_ch,
        n_ch,
        sr,
        cli.output.display(),
        cli.target,
        cli.drive,
        cli.mix,
        cli.cut_dry_lows,
        cli.bypass,
    );
    Ok(())
}
