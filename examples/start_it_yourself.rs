//! Start the bridge in a game's prefix from your own program, with nothing
//! installed on the user's machine.
//!
//! This is the half people trip over. The usual instruction is protontricks,
//! which is a dependency — and on an immutable distribution it arrives as a
//! Flatpak whose private `/dev/shm` makes the bridge publish where nothing can
//! read it. Steam already has the right Proton and says so in the prefix it
//! built, so none of that is needed.
//!
//! ```bash
//! cargo run --example start_it_yourself -- 244210 ./wineshm.exe
//! ```

// Linux only: everything below reads `/dev/shm` and Steam's own layout, and
// the Windows build of this crate has neither. Kept compiling on both so that
// `cargo clippy --all-targets --target x86_64-pc-windows-gnu` — which is how
// the shipped binary is checked — does not trip over the examples.
#[cfg(not(unix))]
fn main() {
    eprintln!("this example is for the Linux side; run it there");
}

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, ExitCode};
#[cfg(unix)]
use wineshm::launch::{Launch, how_to_launch, inside_flatpak};

#[cfg(unix)]
fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(app_id), Some(exe)) = (args.next(), args.next()) else {
        eprintln!("usage: start_it_yourself APPID PATH_TO_wineshm.exe");
        eprintln!("       (Assetto Corsa is 244210)");
        return ExitCode::from(2);
    };
    let Ok(app_id) = app_id.parse::<u32>() else {
        eprintln!("{app_id} is not a Steam app id");
        return ExitCode::from(2);
    };
    let exe = PathBuf::from(exe);

    if inside_flatpak() {
        eprintln!(
            "warning: this is running inside a Flatpak, which has a /dev/shm of its own — \
             the blocks will not be visible outside the sandbox"
        );
    }

    let how = how_to_launch(app_id);
    match &how {
        Launch::Proton { wine, prefix } => {
            println!("prefix: {}", prefix.display());
            println!("proton: {}", wine.display());
            println!("nothing needed installing");
        }
        Launch::Protontricks { .. } => {
            println!("no Steam prefix found for {app_id}");
            println!("falling back to protontricks-launch, which has to be installed");
        }
    }

    let (program, mut args) = how.command(&exe);
    args.push("--preset".to_string());
    args.push("assetto-corsa".to_string());

    let mut command = Command::new(&program);
    command.args(&args);
    for (key, value) in how.env() {
        command.env(key, value);
    }
    // The folder the program is in: Wine resolves a path against the prefix's
    // drive mappings, and one that is not mapped — `/tmp` — comes back as
    // "file not found" with the file plainly there.
    if let Some(dir) = Launch::working_dir(&exe) {
        command.current_dir(dir);
    }

    println!("running: {} {:?}", program.display(), args);
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("the bridge exited with {status}");
            ExitCode::FAILURE
        }
        Err(why) => {
            eprintln!("could not start {}: {why}", program.display());
            ExitCode::FAILURE
        }
    }
}
