//! The command line, parsed by hand.
//!
//! **Hand-rolled because the alternative is charged to the user.** An argument
//! parser is a fine dependency in a program that starts once on a fast
//! machine; this one is cross-compiled, shipped inside somebody's game folder
//! and started under Wine while they are waiting to drive. There are eight
//! flags. They do not need a crate.
//!
//! All of it is a pure function from arguments to a decision, which is the
//! other reason: it can be tested exhaustively without running anything.

use crate::pacing;
use crate::page::Page;
use core::time::Duration;
use std::path::PathBuf;

/// What the program was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Publish the pages and hold them.
    Run,
    /// Print the help and stop.
    Help,
    /// Print the version and stop.
    Version,
    /// Report what is already published here, and stop.
    Verify,
    /// Find this Steam game's prefix, and start the Windows build of this
    /// program inside it. Linux side.
    Launch {
        /// Steam's number for the game.
        app_id: u32,
    },
}

/// Everything the program needs to know, once the arguments are read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// What to do.
    pub action: Action,
    /// Which pages to publish.
    pub pages: Vec<Page>,
    /// Where to publish them.
    pub dir: PathBuf,
    /// The pace while something is changing.
    pub quick: Duration,
    /// The slowest a quiet page is looked at.
    pub slowest: Duration,
    /// Say nothing but errors.
    pub quiet: bool,
    /// Report as JSON rather than as a table, for a program reading this.
    pub json: bool,
    /// The block that changes while the writer is alive. When it goes quiet,
    /// every block is zeroed — see [`crate::watchdog`].
    pub heartbeat: Option<String>,
    /// How long that block may be quiet first.
    pub blank_after: Duration,
    /// Which `wineshm.exe` to start, when launching. `None` means the one
    /// sitting beside this program.
    pub exe: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            action: Action::Run,
            pages: Vec::new(),
            dir: PathBuf::from(crate::store::SHM_DIR),
            quick: pacing::QUICK,
            slowest: pacing::SLOWEST,
            quiet: false,
            json: false,
            heartbeat: None,
            blank_after: crate::watchdog::BLANK_AFTER,
            exe: None,
        }
    }
}

/// The help, which is also the documentation of the flags.
pub const HELP: &str = concat!(
    "wineshm ",
    env!("CARGO_PKG_VERSION"),
    " — expose a Windows program's named shared memory to Linux.

Run this inside the Wine or Proton prefix the Windows program runs in. Every
page it publishes appears as a file under /dev/shm, which any Linux program
can open and read.

USAGE:
    On Linux — finds the prefix and starts the Windows build inside it:
    wineshm --appid 244210 --preset assetto-corsa

    Inside the prefix, if you are starting it yourself:
    wineshm.exe --preset assetto-corsa
    wineshm.exe --page acpmf_physics:2048 --page acpmf_graphics:2048

OPTIONS:
    --appid N            Steam's number for the game. Finds its Proton prefix
                         and starts wineshm.exe in it. Nothing to install.
    --exe PATH           Which wineshm.exe to start [default: beside this one]
    --preset NAME        A ready-made page list. One of: assetto-corsa, rfactor2
    --page NAME:SIZE     One page. Repeatable. Size may carry K or M.
    --dir PATH           Where to publish [default: /dev/shm]
    --heartbeat NAME     The block that changes while the writer is alive. When
                         it goes quiet every block is zeroed, so a game that has
                         exited stops reading as a live session. A preset sets
                         this itself; give it only to override
    --blank-after MS     How long it may be quiet first [default: 5000]
    --quick MS           Pace while a mirrored page is changing [default: 4]
    --slowest MS         Slowest pace for a quiet page [default: 64]
    --verify             Report what is published here already, then stop
    --json               With --verify, report as JSON instead of a table
    --quiet              Say nothing but errors
    -h, --help           This
    -V, --version        Print the version

The program holds the pages until it is asked to stop. Type 'exit' or close it.
"
);

/// Read the arguments. The program name must already be gone.
pub fn parse<I>(args: I) -> Result<Options, String>
where
    I: IntoIterator<Item = String>,
{
    let mut options = Options::default();
    let mut presets_used = false;
    let mut preset_named: Option<String> = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        // `--flag=value` and `--flag value` both work, because people type
        // both and being told off for the wrong one is not help.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (arg, None),
        };

        let mut value = |flag: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{flag} needs a value"))
        };

        match flag.as_str() {
            "-h" | "--help" => {
                return Ok(Options {
                    action: Action::Help,
                    ..options
                });
            }
            "-V" | "--version" => {
                return Ok(Options {
                    action: Action::Version,
                    ..options
                });
            }
            "--verify" => options.action = Action::Verify,
            "--exe" => options.exe = Some(PathBuf::from(value("--exe")?)),
            "--appid" => {
                let text = value("--appid")?;
                let app_id = text.trim().parse().map_err(|_| {
                    format!("--appid wants Steam's number for the game, not {text:?}")
                })?;
                options.action = Action::Launch { app_id };
            }
            "--quiet" => options.quiet = true,
            "--json" => options.json = true,
            "--heartbeat" => options.heartbeat = Some(value("--heartbeat")?),
            "--blank-after" => {
                options.blank_after = millis(&value("--blank-after")?, "--blank-after")?
            }
            "--dir" => options.dir = PathBuf::from(value("--dir")?),
            "--quick" => options.quick = millis(&value("--quick")?, "--quick")?,
            "--slowest" => options.slowest = millis(&value("--slowest")?, "--slowest")?,
            "--page" => {
                let text = value("--page")?;
                let page = Page::parse(&text).map_err(|why| format!("--page {text}: {why}"))?;
                options.pages.push(page);
            }
            "--preset" => {
                let name = value("--preset")?;
                let pages = crate::page::preset(&name).ok_or_else(|| {
                    format!(
                        "no preset called {name:?} — there is {}",
                        crate::page::PRESETS.join(", ")
                    )
                })?;
                presets_used = true;
                preset_named = Some(name);
                options.pages.extend(pages);
            }
            other => {
                return Err(format!("{other} is not a flag this knows; try --help"));
            }
        }
    }

    // A page asked for twice is a mistake worth naming rather than a section
    // created twice: the second `CreateFileMapping` hands back the first, and
    // the program would report two pages while serving one.
    if let Some(twice) = first_repeat(&options.pages) {
        return Err(format!("{twice} was asked for more than once"));
    }

    // Launching passes the pages through to the copy it starts, so it needs
    // them for the same reason running does.
    let needs_pages = matches!(options.action, Action::Run | Action::Launch { .. });
    // **A preset knows which of its blocks moves, so it says so.** Without
    // this every caller has to know that Assetto Corsa's physics block is the
    // live one and its static block is not — and getting that wrong blanks a
    // session that is still running. An explicit `--heartbeat` still wins.
    if options.heartbeat.is_none()
        && let Some(name) = preset_named.as_deref()
        && let Some(beat) = crate::page::preset_heartbeat(name)
    {
        options.heartbeat = Some(beat.to_string());
    }

    // A heartbeat naming a block that is not being published watches nothing,
    // for ever, and silently: exactly the shape of a typo nobody finds.
    if let Some(name) = options.heartbeat.as_deref()
        && !options.pages.iter().any(|page| page.name == name)
    {
        return Err(format!(
            "--heartbeat {name} is not one of the pages being published"
        ));
    }

    if needs_pages && options.pages.is_empty() {
        return Err(if presets_used {
            "that preset is empty".to_string()
        } else {
            "nothing to publish — give --preset or at least one --page".to_string()
        });
    }

    Ok(options)
}

fn millis(text: &str, flag: &str) -> Result<Duration, String> {
    let value: u64 = text
        .trim()
        .parse()
        .map_err(|_| format!("{flag} wants a number of milliseconds, not {text:?}"))?;
    Ok(Duration::from_millis(value))
}

fn first_repeat(pages: &[Page]) -> Option<&str> {
    pages.iter().enumerate().find_map(|(at, page)| {
        pages[..at]
            .iter()
            .any(|earlier| earlier.name == page.name)
            .then_some(page.name.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Result<Options, String> {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn a_preset_fills_the_page_list() {
        let options = parse_of(&["--preset", "assetto-corsa"]).expect("parses");
        assert_eq!(options.action, Action::Run);
        assert_eq!(options.pages.len(), 4);
        assert!(options.pages.iter().any(|p| p.name == "acpmf_physics"));
    }

    #[test]
    fn pages_can_be_given_one_at_a_time() {
        let options = parse_of(&["--page", "a:16", "--page", "b:32"]).expect("parses");
        assert_eq!(options.pages.len(), 2);
        assert_eq!(options.pages[1].bytes, 32);
    }

    #[test]
    fn a_preset_and_extra_pages_add_up() {
        let options =
            parse_of(&["--preset", "assetto-corsa", "--page", "extra:64"]).expect("parses");
        assert_eq!(options.pages.len(), 5);
    }

    #[test]
    fn both_ways_of_writing_a_flag_work() {
        let spaced = parse_of(&["--page", "a:16", "--dir", "/tmp"]).expect("spaced");
        let joined = parse_of(&["--page=a:16", "--dir=/tmp"]).expect("joined");
        assert_eq!(spaced, joined);
    }

    #[test]
    fn help_and_version_win_over_everything_else() {
        assert_eq!(parse_of(&["--help"]).map(|o| o.action), Ok(Action::Help));
        assert_eq!(parse_of(&["-h"]).map(|o| o.action), Ok(Action::Help));
        assert_eq!(parse_of(&["-V"]).map(|o| o.action), Ok(Action::Version));
        // And they do not need pages, which running does.
        assert!(parse_of(&["--help"]).is_ok());
    }

    #[test]
    fn verify_does_not_need_pages_either() {
        let options = parse_of(&["--verify"]).expect("parses");
        assert_eq!(options.action, Action::Verify);
        assert!(options.pages.is_empty());
    }

    #[test]
    fn running_with_nothing_to_publish_is_refused_with_a_way_forward() {
        let why = parse_of(&[]).expect_err("refused");
        assert!(why.contains("--preset"), "{why}");
        assert!(why.contains("--page"), "{why}");
    }

    /// **A preset brings its own heartbeat.** Otherwise every caller has to
    /// know which of Assetto Corsa's blocks is the live one, and the one that
    /// looks most like an answer — the static block, with the car and the
    /// track in it — is the wrong one.
    #[test]
    fn a_preset_switches_on_its_own_stale_data_protection() {
        let options = parse_of(&["--preset", "assetto-corsa"]).expect("parses");
        assert_eq!(options.heartbeat.as_deref(), Some("acpmf_physics"));

        let options = parse_of(&["--preset", "rfactor2"]).expect("parses");
        assert_eq!(
            options.heartbeat.as_deref(),
            Some("$rFactor2SMMP_Telemetry$")
        );
    }

    /// Asking for one explicitly still wins, whichever order it is given in.
    #[test]
    fn a_heartbeat_given_by_hand_overrides_the_presets() {
        for args in [
            ["--preset", "assetto-corsa", "--heartbeat", "acpmf_graphics"],
            ["--heartbeat", "acpmf_graphics", "--preset", "assetto-corsa"],
        ] {
            let options = parse_of(&args).expect("parses");
            assert_eq!(options.heartbeat.as_deref(), Some("acpmf_graphics"));
        }
    }

    /// Pages named one at a time carry no such knowledge, and nothing is
    /// blanked without being asked.
    #[test]
    fn pages_without_a_preset_get_no_heartbeat_of_their_own() {
        let options = parse_of(&["--page", "a:16"]).expect("parses");
        assert_eq!(options.heartbeat, None);
    }

    #[test]
    fn a_preset_nobody_wrote_lists_the_ones_that_exist() {
        let why = parse_of(&["--preset", "gran-turismo"]).expect_err("refused");
        assert!(why.contains("assetto-corsa"), "{why}");
        assert!(why.contains("rfactor2"), "{why}");
    }

    #[test]
    fn a_flag_that_needs_a_value_and_has_none_says_so() {
        for flag in ["--page", "--preset", "--dir", "--quick", "--slowest"] {
            let why = parse_of(&[flag]).expect_err("refused");
            assert!(why.contains(flag), "{flag}: {why}");
        }
    }

    #[test]
    fn a_page_that_makes_no_sense_names_itself_in_the_refusal() {
        let why = parse_of(&["--page", "nonsense"]).expect_err("refused");
        assert!(why.contains("nonsense"), "{why}");
    }

    #[test]
    fn an_unknown_flag_is_refused_rather_than_ignored() {
        let why = parse_of(&["--turbo"]).expect_err("refused");
        assert!(why.contains("--turbo"), "{why}");
        assert!(why.contains("--help"), "{why}");
    }

    /// The second `CreateFileMapping` for one name hands back the first, so a
    /// repeat would be reported as two pages and served as one.
    #[test]
    fn the_same_page_twice_is_refused() {
        let why = parse_of(&["--page", "a:16", "--page", "a:32"]).expect_err("refused");
        assert!(why.contains('a'), "{why}");

        // Including when one of them came from a preset.
        let why =
            parse_of(&["--preset", "ac", "--page", "acpmf_physics:2048"]).expect_err("refused");
        assert!(why.contains("acpmf_physics"), "{why}");
    }

    #[test]
    fn the_pace_can_be_set_and_is_in_milliseconds() {
        let options =
            parse_of(&["--page", "a:16", "--quick", "2", "--slowest", "100"]).expect("parses");
        assert_eq!(options.quick, Duration::from_millis(2));
        assert_eq!(options.slowest, Duration::from_millis(100));
    }

    #[test]
    fn a_pace_that_is_not_a_number_says_which_flag() {
        let why = parse_of(&["--page", "a:16", "--quick", "fast"]).expect_err("refused");
        assert!(why.contains("--quick"), "{why}");
        assert!(why.contains("fast"), "{why}");
    }

    #[test]
    fn the_defaults_are_the_ones_the_help_promises() {
        let options = parse_of(&["--page", "a:16"]).expect("parses");
        assert_eq!(options.dir, PathBuf::from("/dev/shm"));
        assert_eq!(options.quick, Duration::from_millis(4));
        assert_eq!(options.slowest, Duration::from_millis(64));
        assert!(!options.quiet);

        assert!(HELP.contains("/dev/shm"), "the help names another default");
        assert!(HELP.contains("default: 4"), "{HELP}");
        assert!(HELP.contains("default: 64"), "{HELP}");
    }

    /// The help lists the presets, and the list is the one `page` has.
    #[test]
    fn the_help_names_every_preset_that_exists() {
        for name in crate::page::PRESETS {
            assert!(HELP.contains(name), "the help does not mention {name}");
        }
    }

    #[test]
    fn quiet_is_off_until_it_is_asked_for() {
        assert!(!parse_of(&["--page", "a:16"]).expect("parses").quiet);
        assert!(
            parse_of(&["--page", "a:16", "--quiet"])
                .expect("parses")
                .quiet
        );
    }

    #[test]
    fn a_directory_of_your_own_is_taken() {
        let options = parse_of(&["--page", "a:16", "--dir", "/tmp/here"]).expect("parses");
        assert_eq!(options.dir, PathBuf::from("/tmp/here"));
    }
}
