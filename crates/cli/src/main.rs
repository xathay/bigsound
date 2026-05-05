//! BigSound CLI — command-line client for the BigSound daemon.
//!
//! Parameters:
//!   bigsound list                          # all parameters
//!   bigsound get   bigloud:amount
//!   bigsound set   bigloud:amount 0.5
//!   bigsound show                          # all params + values
//!
//! Profiles:
//!   bigsound profile list                  # all profile names
//!   bigsound profile show <name>           # full JSON of one profile
//!   bigsound profile apply <name>          # write profile params to current cache
//!   bigsound profile save  <name>          # snapshot current cache as a user profile
//!   bigsound profile delete <name>         # remove a user-saved profile
//!   bigsound profile active                # which profile was last applied

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use zbus::blocking::{Connection, Proxy};

const SERVICE: &str = "com.bigcommunity.BigSound1";
const PATH: &str = "/com/bigcommunity/BigSound1";
const IFACE: &str = "com.bigcommunity.BigSound1";

#[derive(Parser, Debug)]
#[command(
    name = "bigsound",
    about = "Tune BigSound DSP live; manage device profiles"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List every public parameter the daemon exposes
    List,
    /// Print one parameter's current value
    Get { name: String },
    /// Write a parameter (e.g. `bigsound set bigloud:amount 0.5`)
    Set {
        name: String,
        #[arg(allow_hyphen_values = true)]
        value: f64,
    },
    /// Print every parameter with its current value
    Show,
    /// Profile management
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileCmd {
    /// List all known profile names
    List,
    /// Print a profile's JSON definition
    Show { name: String },
    /// Apply a profile's params to the live DSP
    Apply { name: String },
    /// Snapshot the current parameter cache into a new user profile
    Save { name: String },
    /// Remove a user-saved profile
    Delete { name: String },
    /// Print the name of the last applied profile (auto or manual)
    Active,
}

/// Restore the default SIGPIPE disposition so that piping into `head`, `less`,
/// etc. terminates the process silently with status 141 instead of panicking
/// inside `println!`. Rust ignores SIGPIPE by default, which turns broken-pipe
/// writes into `io::ErrorKind::BrokenPipe`; the `print*!` macros then panic.
#[cfg(unix)]
fn reset_sigpipe() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe() {}

fn main() -> Result<()> {
    reset_sigpipe();
    let cli = Cli::parse();
    let conn = Connection::session().context("connecting to D-Bus session bus")?;
    let proxy = Proxy::new(&conn, SERVICE, PATH, IFACE)
        .context("creating proxy — is bigsound-daemon running?")?;

    match cli.cmd {
        Cmd::List => {
            let names: Vec<String> = proxy.call("List", &()).context("calling List")?;
            for n in names {
                println!("{n}");
            }
        }
        Cmd::Get { name } => {
            let v: f64 = proxy
                .call("Get", &(name.as_str(),))
                .with_context(|| format!("Get({name})"))?;
            println!("{v}");
        }
        Cmd::Set { name, value } => {
            proxy
                .call::<_, _, ()>("Set", &(name.as_str(), value))
                .with_context(|| format!("Set({name}, {value})"))?;
        }
        Cmd::Show => {
            let names: Vec<String> = proxy.call("List", &()).context("calling List")?;
            let width = names.iter().map(|s| s.len()).max().unwrap_or(0);
            for n in names {
                let v: f64 = proxy
                    .call("Get", &(n.as_str(),))
                    .unwrap_or(f64::NAN);
                println!("  {n:<width$}  =  {v}", width = width);
            }
        }
        Cmd::Profile { cmd } => match cmd {
            ProfileCmd::List => {
                let names: Vec<String> = proxy
                    .call("ListProfiles", &())
                    .context("calling ListProfiles")?;
                for n in names {
                    println!("{n}");
                }
            }
            ProfileCmd::Show { name } => {
                let json: String = proxy
                    .call("GetProfile", &(name.as_str(),))
                    .with_context(|| format!("GetProfile({name})"))?;
                println!("{json}");
            }
            ProfileCmd::Apply { name } => {
                proxy
                    .call::<_, _, ()>("ApplyProfile", &(name.as_str(),))
                    .with_context(|| format!("ApplyProfile({name})"))?;
                println!("applied profile '{name}'");
            }
            ProfileCmd::Save { name } => {
                proxy
                    .call::<_, _, ()>("SaveProfile", &(name.as_str(),))
                    .with_context(|| format!("SaveProfile({name})"))?;
                println!("saved current params as profile '{name}'");
            }
            ProfileCmd::Delete { name } => {
                proxy
                    .call::<_, _, ()>("DeleteProfile", &(name.as_str(),))
                    .with_context(|| format!("DeleteProfile({name})"))?;
                println!("deleted user profile '{name}'");
            }
            ProfileCmd::Active => {
                let v: String = proxy
                    .get_property("ActiveProfile")
                    .context("reading ActiveProfile property")?;
                if v.is_empty() {
                    println!("(no profile applied yet)");
                } else {
                    println!("{v}");
                }
            }
        },
    }
    Ok(())
}
