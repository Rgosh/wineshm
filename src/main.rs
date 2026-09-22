//! The program. Publish the pages, hold them, put them away.

use std::process::ExitCode;
use wineshm::Store;
use wineshm::cli::{Action, Options};

// Everything below is reached only from the Windows half — the Linux build of
// this binary exists to be cross-compiled from and to run its own tests.
#[cfg(windows)]
use std::io::BufRead;
#[cfg(windows)]
use std::sync::Arc;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::time::{Duration, Instant};
#[cfg(windows)]
use wineshm::Page;

fn main() -> ExitCode {
    let options = match wineshm::cli::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("wineshm: {why}");
            return ExitCode::from(2);
        }
    };

    match options.action {
        Action::Help => {
            print!("{}", wineshm::cli::HELP);
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("wineshm {}", wineshm::VERSION);
            ExitCode::SUCCESS
        }
        Action::Verify => match verify(&options) {
            Ok(found) => {
                if found {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(why) => {
                eprintln!("wineshm: {why}");
                ExitCode::FAILURE
            }
        },
        Action::Launch { app_id } => match launch(&options, app_id) {
            Ok(code) => code,
            Err(why) => {
                eprintln!("wineshm: {why}");
                ExitCode::FAILURE
            }
        },
        Action::Run => match run(&options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(why) => {
                eprintln!("wineshm: {why}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Report what is already published in this directory.
///
/// Runs on Linux as happily as under Wine, because reading the note and the
/// files is ordinary file work — which is the point of publishing a note at
/// all.
fn verify(options: &Options) -> std::io::Result<bool> {
    let store = Store::at(&options.dir);
    let note_path = store.dir().join(wineshm::announce::FILE);

    let Ok(text) = std::fs::read_to_string(&note_path) else {
        println!("nothing is published in {}", store.dir().display());
        return Ok(false);
    };

    let Some(note) = wineshm::Note::parse(&text) else {
        println!(
            "{} is there but is not a note this understands",
            note_path.display()
        );
        return Ok(false);
    };

    println!("wineshm {} is publishing, pid {}", note.version, note.pid);
    if !note.is_understood() {
        println!(
            "  note format {} — this build reads {}",
            note.format,
            wineshm::announce::FORMAT
        );
    }
    println!("  {:<32} {:>10}  {:<9} ON DISK", "PAGE", "BYTES", "MODE");
    for (page, mode) in &note.pages {
        let on_disk = std::fs::metadata(store.path(page))
            .map(|meta| {
                let len = meta.len();
                if len == page.bytes as u64 {
                    format!("{len} bytes")
                } else {
                    format!("{len} bytes — SHOULD BE {}", page.bytes)
                }
            })
            .unwrap_or_else(|_| "MISSING".to_string());
        println!(
            "  {:<32} {:>10}  {:<9} {on_disk}",
            page.name, page.bytes, mode
        );
    }
    Ok(true)
}

/// Find the game's prefix and start the Windows build of this program in it.
///
/// **The whole point of this flag.** The instruction everybody is given is
/// `protontricks-launch`, which is a dependency — and on Bazzite, Silverblue
/// or a Steam Deck it usually arrives as a Flatpak, whose private `/dev/shm`
/// makes the bridge publish into a sandbox nothing outside can read. Steam
/// already has the right Proton on disk and says so in the prefix it built,
/// so none of that is necessary.
#[cfg(unix)]
fn launch(options: &Options, app_id: u32) -> std::io::Result<ExitCode> {
    use std::process::Command;
    use wineshm::launch::{Launch, how_to_launch, inside_flatpak};

    if inside_flatpak() && !options.quiet {
        eprintln!(
            "wineshm: this is running inside a Flatpak, which has a /dev/shm of its own — \
             the pages will not be visible outside the sandbox"
        );
    }

    let exe = match options.exe.clone() {
        Some(given) => given,
        None => beside_this_program("wineshm.exe").ok_or_else(|| {
            std::io::Error::other("no wineshm.exe beside this program — say where with --exe")
        })?,
    };
    if !exe.is_file() {
        return Err(std::io::Error::other(format!(
            "{} is not there",
            exe.display()
        )));
    }

    let how = how_to_launch(app_id);
    let (program, mut args) = how.command(&exe);
    // Everything this was asked for goes through to the copy inside the
    // prefix, minus the flags that got us here.
    for page in &options.pages {
        args.push("--page".to_string());
        args.push(format!("{}:{}", page.name, page.bytes));
    }
    args.push("--dir".to_string());
    args.push(options.dir.to_string_lossy().into_owned());
    if options.quiet {
        args.push("--quiet".to_string());
    }

    if !options.quiet {
        match &how {
            Launch::Proton { wine, prefix } => {
                println!("prefix: {}", prefix.display());
                println!("proton: {}", wine.display());
            }
            Launch::Protontricks { .. } => {
                println!(
                    "no Steam prefix found for {app_id} — falling back to protontricks-launch"
                );
            }
        }
    }

    let mut command = Command::new(&program);
    command.args(&args);
    for (key, value) in how.env() {
        command.env(key, value);
    }
    if let Some(dir) = Launch::working_dir(&exe) {
        command.current_dir(dir);
    }

    let status = command.status().map_err(|why| match why.kind() {
        std::io::ErrorKind::NotFound => std::io::Error::other(format!(
            "{} is not there — and no Steam Proton was found for {app_id} either",
            program.display()
        )),
        _ => why,
    })?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(not(unix))]
fn launch(_options: &Options, _app_id: u32) -> std::io::Result<ExitCode> {
    Err(std::io::Error::other(
        "--appid finds a Steam Proton prefix, which is a Linux thing; inside the prefix, \
         run this without it",
    ))
}

/// A file sitting next to this executable.
#[cfg(unix)]
fn beside_this_program(name: &str) -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(name))
}

#[cfg(not(windows))]
fn run(_options: &Options) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "this has to run inside the Wine or Proton prefix, as a Windows program — \
         build it for x86_64-pc-windows-gnu and start it in there",
    ))
}

#[cfg(windows)]
fn run(options: &Options) -> std::io::Result<()> {
    use wineshm::win::Published;

    let store = Store::at(&options.dir);
    if !store.is_usable() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory — on Linux this is /dev/shm, and under Wine it has to be \
             reachable from inside the prefix",
            store.dir().display()
        )));
    }

    let pacing = wineshm::Pacing::new(options.quick, options.slowest);
    let mut published: Vec<Published> = Vec::with_capacity(options.pages.len());
    for page in &options.pages {
        match Published::open(&store, page.clone(), pacing.clone()) {
            Ok(one) => published.push(one),
            Err(why) => {
                // Put back what was already published rather than leaving half
                // a set behind for the next run to inherit.
                let done: Vec<Page> = published.iter().map(|p| p.page().clone()).collect();
                drop(published);
                let _ = store.remove_all(&done);
                return Err(why);
            }
        }
    }

    let note = wineshm::Note {
        version: wineshm::VERSION.to_string(),
        format: wineshm::announce::FORMAT,
        pid: std::process::id(),
        pages: published
            .iter()
            .map(|one| (one.page().clone(), one.mode()))
            .collect(),
    };
    let note_path = store.dir().join(wineshm::announce::FILE);
    std::fs::write(&note_path, note.render())?;

    if !options.quiet {
        announce_to_the_terminal(&published, &store);
    }

    hold(&mut published, options);

    if !options.quiet {
        report_what_it_cost(&published);
    }

    // Everything goes, and the note goes first: while it is there, a reader
    // takes it as a promise that the pages are too.
    let _ = std::fs::remove_file(&note_path);
    let pages: Vec<Page> = published.iter().map(|one| one.page().clone()).collect();
    drop(published);

    let failures = store.remove_all(&pages);
    if !options.quiet {
        summarise(&pages, &store, &failures);
    }
    for (name, why) in &failures {
        eprintln!("wineshm: {name} could not be removed: {why}");
    }
    Ok(())
}

#[cfg(windows)]
fn announce_to_the_terminal(published: &[wineshm::win::Published], store: &Store) {
    println!(
        "wineshm {} publishing in {}",
        wineshm::VERSION,
        store.dir().display()
    );
    println!("  {:<32} {:>10}  MODE", "PAGE", "BYTES");
    for one in published {
        println!(
            "  {:<32} {:>10}  {}",
            one.page().name,
            one.page().bytes,
            one.mode()
        );
    }
    let mirrored = published
        .iter()
        .filter(|one| one.mode() == wineshm::Mode::Mirrored)
        .count();
    if mirrored == 0 {
        println!("every page is owned — nothing will be copied while this runs");
    } else {
        println!(
            "{mirrored} of {} pages are mirrored, because the writer was already running",
            published.len()
        );
    }
}

/// What the mirroring actually cost, per page.
///
/// **Printed rather than claimed.** The saving from comparing before copying
/// and from backing off depends entirely on how much the writer wrote, which
/// is a property of somebody else's program on somebody else's evening. So the
/// program counts it and says so, and a number in a readme can be checked
/// against a number on the screen.
#[cfg(windows)]
fn report_what_it_cost(published: &[wineshm::win::Published]) {
    let mirrored: Vec<&wineshm::win::Published> = published
        .iter()
        .filter(|one| one.mode() == wineshm::Mode::Mirrored)
        .collect();
    if mirrored.is_empty() {
        return;
    }

    println!();
    println!(
        "  {:<32} {:>8} {:>8} {:>7}  {}",
        "MIRRORED PAGE", "LOOKS", "COPIES", "WORK", "PACE"
    );
    let (mut looks, mut copies, mut saved) = (0u64, 0u64, 0u64);
    for one in &mirrored {
        let shadow = one.shadow();
        looks += shadow.looks();
        copies += shadow.copies();
        saved += shadow
            .bytes_if_always_copied()
            .saturating_sub(shadow.bytes_copied());
        println!(
            "  {:<32} {:>8} {:>8} {:>6.1}%  every {:.0} ms",
            one.page().name,
            shadow.looks(),
            shadow.copies(),
            shadow.work_share() * 100.0,
            one.pacing().interval().as_secs_f64() * 1000.0,
        );
    }
    if looks > 0 {
        println!(
            "  {copies} copies in {looks} looks — {:.1}% of them did any work, {} KiB not copied",
            100.0 * copies as f64 / looks as f64,
            saved / 1024
        );
    }
}

#[cfg(windows)]
fn summarise(pages: &[Page], _store: &Store, failures: &[(String, std::io::Error)]) {
    println!(
        "put back {} of {} pages",
        pages.len() - failures.len(),
        pages.len()
    );
}

/// Hold the pages until somebody says stop, copying the mirrored ones.
#[cfg(windows)]
fn hold(published: &mut [wineshm::win::Published], options: &Options) {
    let stop = Arc::new(AtomicBool::new(false));
    watch_input(Arc::clone(&stop), options.quiet);

    // A deadline per page, because each one backs off at its own rate: a
    // static block that never changes should not keep a physics block's pace.
    let mut due: Vec<Instant> = vec![Instant::now(); published.len()];
    let mut work = published
        .iter()
        .any(|one| one.mode() == wineshm::Mode::Mirrored);

    while !stop.load(Ordering::Relaxed) {
        if !work {
            // Nothing to copy, ever. Sleep on the stop flag instead of
            // spinning: an owned set costs this process nothing at all.
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        let now = Instant::now();
        let mut soonest = now + options.slowest;
        for (at, one) in published.iter_mut().enumerate() {
            if one.mode() != wineshm::Mode::Mirrored {
                continue;
            }
            if due[at] <= now {
                let wait = one.tick();
                due[at] = now + wait;
            }
            soonest = soonest.min(due[at]);
        }
        work = true;
        let nap = soonest.saturating_duration_since(Instant::now());
        if !nap.is_zero() {
            std::thread::sleep(nap);
        }
    }
}

/// Watch standard input for a reason to stop.
///
/// See [`wineshm::win::stdin_is_a_console`] for why this is not simply "input
/// ended, so exit".
#[cfg(windows)]
fn watch_input(stop: Arc<AtomicBool>, quiet: bool) {
    let console = wineshm::win::stdin_is_a_console();
    if !quiet {
        if console {
            println!("type 'exit' to stop, or close this window");
        } else {
            println!("running until this process is closed");
        }
    }

    std::thread::spawn(move || {
        let began = Instant::now();
        let mut line = String::new();
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        loop {
            line.clear();
            match handle.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    // Input that ends the instant it is asked, with no console
                    // on the other end, is nobody there — not somebody asking
                    // this to stop. Anything later is a parent that has gone.
                    let nobody = !console && began.elapsed() < Duration::from_secs(2);
                    if !nobody {
                        stop.store(true, Ordering::Relaxed);
                    }
                    return;
                }
                Ok(_) => {
                    if line.trim().eq_ignore_ascii_case("exit") {
                        stop.store(true, Ordering::Relaxed);
                        return;
                    }
                    println!("not a command: {:?} — type 'exit' to stop", line.trim());
                }
            }
        }
    });
}
