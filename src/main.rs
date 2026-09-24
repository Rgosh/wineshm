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

    let pulse = wineshm::reader::pulse(&store);

    if options.json {
        print!("{}", note.to_json(pulse));
        return Ok(pulse.is_none_or(wineshm::Pulse::is_worth_reading));
    }

    println!("wineshm {} is publishing, pid {}", note.version, note.pid);
    // **The line that separates a bridge from its leftovers.** A bridge that
    // was killed leaves this file and its pages exactly as they were, and
    // every number in them reads as real — which is the worst way for this to
    // fail, because nothing looks wrong.
    if let Some(pulse) = pulse
        && !pulse.is_worth_reading()
    {
        println!("  {}", pulse.describe());
    }
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
    Ok(pulse.is_none_or(wineshm::Pulse::is_worth_reading))
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
    // **The one exit the Windows side cannot tidy after itself.** Wine turns
    // a Ctrl-C into a console control event, so `wineshm::shutdown` catches
    // that, a closed window, a logoff and a shutdown. It does not translate
    // SIGTERM — measured, not assumed — and SIGTERM is what a script, a
    // session manager and Steam all send. The Windows process is killed where
    // it stands and its pages stay behind holding the last frame.
    //
    // This side outlives it and knows exactly which pages it asked for, so it
    // can finish the job. Only when nothing is publishing there any more: a
    // note with a pulse means somebody else is serving those files, and
    // removing them would break a bridge that is working.
    sweep_after_the_prefix(options);

    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Take away what a killed bridge left, having checked nothing is using it.
///
/// **The wait is the whole of it.** A bridge writes its note every couple of
/// seconds so that a reader can tell a running one from a killed one — see
/// [`wineshm::liveness`] — and the note of the bridge that has just died is by
/// definition only a moment old. Asking "is anything publishing here?" the
/// instant the child exits therefore always answers yes, about the corpse.
///
/// So wait for the pulse to stop. A note that goes stale was the child's and
/// the pages under it are dead; a note that keeps being touched belongs to
/// something else that is alive, and taking its pages away would break a
/// bridge that is working. The wait costs nothing in the ordinary case,
/// because a bridge that shut down cleanly took its note with it and there is
/// nothing here to wait for.
#[cfg(unix)]
fn sweep_after_the_prefix(options: &Options) {
    let store = Store::at(&options.dir);

    let give_up_at =
        std::time::Instant::now() + wineshm::liveness::STALE + std::time::Duration::from_secs(2);
    while wineshm::reader::live_publishing(&store).is_some() {
        if std::time::Instant::now() >= give_up_at {
            // Still beating after longer than a note can go untouched: somebody
            // else is publishing here.
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    let note = store.dir().join(wineshm::announce::FILE);
    let had_note = note.is_file();
    let _ = std::fs::remove_file(&note);

    let left: Vec<&wineshm::Page> = options
        .pages
        .iter()
        .filter(|page| store.path(page).exists())
        .collect();
    if left.is_empty() && !had_note {
        return;
    }

    let owned: Vec<wineshm::Page> = left.iter().map(|page| (*page).clone()).collect();
    let failures = store.remove_all(&owned);
    if !options.quiet && !owned.is_empty() {
        println!(
            "the bridge inside the prefix did not shut down cleanly — took away {} page{} it \
             left behind, so what is left of that session does not read as a live one",
            owned.len() - failures.len(),
            if owned.len() == 1 { "" } else { "s" }
        );
    }
    for (name, why) in &failures {
        eprintln!("wineshm: {name} could not be removed: {why}");
    }
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

    // **Another bridge already publishing here is a reason to stop.** Two of
    // them over one directory is not a redundancy: the second zeroes the files
    // the first is serving, and from then on whichever writes last wins. It
    // presents as telemetry that flickers between real and blank, and it is
    // what happens when a window is closed without the program noticing.
    //
    // A note left by a bridge that *died* is not this: it has no pulse, and
    // being unable to start over one of those would be worse than the fault.
    if let Some(note) = wineshm::reader::live_publishing(&store) {
        return Err(std::io::Error::other(format!(
            "wineshm {} is already publishing in {} (pid {}) — stop it first, or publish \
             somewhere else with --dir",
            note.version,
            store.dir().display(),
            note.pid
        )));
    }

    // **Before anything is published, not after.** The window can be closed
    // during start-up as easily as during a session, and the pages that exist
    // by then are exactly the ones that would be left behind.
    let warned = wineshm::shutdown::listen();

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
        // Said only when it is not true. A program that announces every
        // faculty it has teaches people to skip its output, and this one line
        // is the difference between closing the window being tidy and closing
        // it leaving a dead session behind.
        if !warned {
            println!(
                "note: this system did not let wineshm ask to be told before it is killed, so                  closing this window will leave the pages behind. Type 'exit' instead."
            );
        }
    }

    hold(&mut published, options, &note_path, &note);

    if !options.quiet {
        report_what_it_cost(&published);
    }

    if let Some(why) = wineshm::shutdown::why()
        && !options.quiet
    {
        println!("{} — putting the pages back", why.told());
    }

    // Everything goes, and the note goes first: while it is there, a reader
    // takes it as a promise that the pages are too.
    let _ = std::fs::remove_file(&note_path);

    // **Zeroed before they are unlinked, so that failing to unlink still
    // leaves nothing readable.** Removing the file is what normally ends it,
    // but a page this process could not remove — a directory somebody else
    // owns, a `--dir` on a read-only mount — would otherwise stay behind
    // holding the last frame, which is the state this program spends the rest
    // of its effort avoiding.
    for one in published.iter_mut() {
        one.blank();
    }

    let pages: Vec<Page> = published.iter().map(|one| one.page().clone()).collect();
    drop(published);

    let failures = store.remove_all(&pages);
    if !options.quiet {
        summarise(&pages, &store, &failures);
    }
    for (name, why) in &failures {
        eprintln!("wineshm: {name} could not be removed: {why}");
    }

    // The handler, if one is listening, is holding the process open waiting
    // for exactly this.
    wineshm::shutdown::finished();
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
        "  {:<32} {:>8} {:>8} {:>7}  PACE",
        "MIRRORED PAGE", "LOOKS", "COPIES", "WORK"
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
fn hold(
    published: &mut [wineshm::win::Published],
    options: &Options,
    note_path: &std::path::Path,
    note: &wineshm::Note,
) {
    use wineshm::Schedule;

    let stop = Arc::new(AtomicBool::new(false));
    watch_input(Arc::clone(&stop), options.quiet);

    // **Owned pages are not in here at all.** There is nothing to look at: the
    // writer's stores land in the file directly, so a set with no mirrored
    // page costs this process one wake-up every tenth of a second, and that
    // only so it notices being asked to stop.
    let mirroring: Vec<usize> = published
        .iter()
        .enumerate()
        .filter(|(_, one)| one.mode() == wineshm::Mode::Mirrored)
        .map(|(at, _)| at)
        .collect();

    // A deadline per page, because each backs off at its own rate: a static
    // block that never changes should not keep a physics block's pace. The
    // arithmetic is in `wineshm::Schedule`, where it can be tested.
    let mut schedule = Schedule::new(mirroring.len());
    let began = Instant::now();
    let ceiling = options.slowest.max(Duration::from_millis(100));

    // The note is rewritten on a slow beat so that its modified time is a
    // pulse — see `wineshm::liveness`. One small write every couple of seconds
    // is the price of a reader being able to tell a running bridge from one
    // that was killed, which is the difference between live telemetry and a
    // session that ended hours ago reading as real.
    // **The writer going away, and the frame it leaves behind.** Nothing
    // else in this program can notice it: the section stays because this
    // process is holding it, and the file goes on holding the last frame for
    // ever. The caller names a block that changes while the writer is alive;
    // when it stops, everything is zeroed. See `wineshm::watchdog`.
    let heartbeat = options
        .heartbeat
        .as_deref()
        .and_then(|name| published.iter().position(|one| one.page().name == name));
    let mut dog = wineshm::Watchdog::new(options.blank_after);
    let mut last_look = Instant::now();

    let mut last_beat = Instant::now();
    let beating = note.render();
    let beat = |last: &mut Instant| {
        if wineshm::liveness::due_to_beat(last.elapsed(), wineshm::liveness::BEAT) {
            let _ = std::fs::write(note_path, &beating);
            *last = Instant::now();
        }
    };

    while !stop.load(Ordering::Relaxed) && !wineshm::shutdown::asked_to_stop() {
        if let Some(at) = heartbeat {
            let since = last_look.elapsed();
            last_look = Instant::now();
            let stirred = published
                .get_mut(at)
                .is_some_and(wineshm::win::Published::stirred);
            match dog.saw(stirred, since) {
                wineshm::watchdog::Verdict::Blank => {
                    for one in published.iter_mut() {
                        one.blank();
                    }
                    if !options.quiet {
                        println!(
                            "nothing has written to {} for {:.0}s — every block zeroed, so what \
                             is left of the last session does not read as this one",
                            options.heartbeat.as_deref().unwrap_or_default(),
                            dog.quiet_for().as_secs_f64()
                        );
                    }
                }
                wineshm::watchdog::Verdict::Writing => {
                    if !options.quiet {
                        println!("writing again");
                    }
                }
                wineshm::watchdog::Verdict::Nothing => {}
            }
        }

        if mirroring.is_empty() {
            // Nothing to copy, and still two things to do: say that this is
            // alive, which a reader has no other way to know, and watch the
            // heartbeat, which nothing else will.
            beat(&mut last_beat);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        beat(&mut last_beat);

        let now = began.elapsed();
        for (slot, page) in mirroring.iter().enumerate() {
            if schedule.is_due(slot, now)
                && let Some(one) = published.get_mut(*page)
            {
                schedule.looked_at(slot, now, one.tick());
            }
        }

        let nap = schedule.nap(began.elapsed(), ceiling);
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
                    // The rule is in `wineshm::schedule`, where it is tested.
                    let nobody = wineshm::schedule::nobody_was_there(
                        console,
                        began.elapsed(),
                        wineshm::schedule::GRACE,
                    );
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
