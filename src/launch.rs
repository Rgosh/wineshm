//! Finding the prefix, and something to run the bridge in it with.
//!
//! **This is the half that actually breaks.** The bridge is a small Windows
//! program and it works everywhere; what does not work is starting it. The
//! usual instruction is `protontricks-launch`, which is a dependency — and on
//! a distribution where you cannot install one it arrives as a Flatpak, and a
//! Flatpak has a `/dev/shm` of its own. The bridge inside it then publishes
//! its pages into a tmpfs that exists only in that sandbox, reports success,
//! and nothing outside can see a byte. From the user's chair that is "running
//! it does nothing".
//!
//! Nothing needs to be installed. Steam already ships the Proton the game is
//! set to use, and writes which one it was into the prefix it built. Read it
//! and the middleman is not needed.
//!
//! Every path-shaped decision here is a pure function over a root directory,
//! so the layouts of distributions nobody here runs — Bazzite, SteamOS, a
//! Flatpak Steam, a Snap Steam, a library on a second disk — are tested by
//! building those trees rather than by owning those machines.

use std::path::{Path, PathBuf};

/// Where Steam might be, under one home directory.
///
/// All five real layouts, in the order they should be believed:
///
/// | | Path under `$HOME` |
/// |---|---|
/// | Native (most distributions) | `.steam/steam`, `.local/share/Steam` |
/// | Flatpak | `.var/app/com.valvesoftware.Steam/.local/share/Steam` |
/// | Flatpak, older layout | `.var/app/com.valvesoftware.Steam/data/Steam` |
/// | Snap | `snap/steam/common/.local/share/Steam` |
///
/// The Steam Deck and Bazzite use the native layout; they are not special
/// cases, which is the point of listing them nowhere.
pub fn steam_roots_under(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        home.join(".steam").join("steam"),
        home.join(".steam").join("root"),
        home.join(".local").join("share").join("Steam"),
        home.join(".var")
            .join("app")
            .join("com.valvesoftware.Steam")
            .join(".local")
            .join("share")
            .join("Steam"),
        home.join(".var")
            .join("app")
            .join("com.valvesoftware.Steam")
            .join("data")
            .join("Steam"),
        home.join("snap")
            .join("steam")
            .join("common")
            .join(".local")
            .join("share")
            .join("Steam"),
    ];
    roots.dedup();
    roots
}

/// The same, for the home directory this process actually has.
pub fn steam_roots() -> Vec<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) => steam_roots_under(Path::new(&home)),
        None => Vec::new(),
    }
}

/// Every library folder named by a `libraryfolders.vdf`.
///
/// A hand-rolled read of one key rather than a VDF parser: `"path"` is the
/// only field needed and it has survived every revision of the format. A
/// dependency to read one string is a dependency in everybody's build.
pub fn library_paths(vdf: &str) -> Vec<PathBuf> {
    vdf.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("\"path\"")?;
            let opened = rest.find('"')? + 1;
            let closed = rest[opened..].find('"')? + opened;
            Some(PathBuf::from(&rest[opened..closed]))
        })
        .collect()
}

/// Every place a game's files or prefix might be, given the Steam roots.
pub fn libraries(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        found.push(root.clone());
        if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf"))
        {
            found.extend(library_paths(&text));
        }
    }
    found.dedup();
    found
}

/// Where a game's Proton prefix would be inside one library.
pub fn prefix_in(library: &Path, app_id: u32) -> PathBuf {
    library
        .join("steamapps")
        .join("compatdata")
        .join(app_id.to_string())
        .join("pfx")
}

/// The prefix Steam built for a game, wherever its library is.
///
/// `None` before the game has been run once: Steam builds the prefix on first
/// launch, not at install.
pub fn prefix_for(app_id: u32) -> Option<PathBuf> {
    libraries(&steam_roots())
        .into_iter()
        .map(|library| prefix_in(&library, app_id))
        .find(|candidate| candidate.is_dir())
}

/// The Proton build a prefix was made by, from the note Proton leaves in it.
///
/// The second line of `config_info` is that Proton's own font directory, and
/// the build is two components above it. Reading the prefix rather than
/// Steam's config means this works for Proton 7, Experimental, GE and
/// anything else, without knowing they exist.
pub fn wine_from_config_info(config_info: &str) -> Option<PathBuf> {
    let fonts = Path::new(config_info.lines().nth(1)?.trim());
    Some(fonts.parent()?.parent()?.join("bin").join("wine"))
}

/// The `wine` that goes with a prefix, if it is still installed.
pub fn wine_for(prefix: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(prefix.parent()?.join("config_info")).ok()?;
    wine_from_config_info(&text).filter(|wine| wine.is_file())
}

/// How to start a Windows program inside a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Steam's own Proton, found in the prefix it built. Nothing to install.
    Proton {
        /// The `wine` binary.
        wine: PathBuf,
        /// What to set `WINEPREFIX` to.
        prefix: PathBuf,
    },
    /// Nothing was found, so fall back to the thing people are told to
    /// install.
    Protontricks {
        /// The app id to hand it.
        app_id: u32,
    },
}

impl Launch {
    /// The program to run and its arguments, for `std::process::Command`.
    pub fn command(&self, exe: &Path) -> (PathBuf, Vec<String>) {
        match self {
            Self::Proton { wine, .. } => (wine.clone(), vec![exe.to_string_lossy().into_owned()]),
            Self::Protontricks { app_id } => (
                PathBuf::from("protontricks-launch"),
                vec![
                    "--appid".to_string(),
                    app_id.to_string(),
                    exe.to_string_lossy().into_owned(),
                ],
            ),
        }
    }

    /// The environment it needs.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            // Both of these stop `winedevice.exe` sitting on a core for the
            // length of the session.
            ("DBUS_FATAL_WARNINGS".to_string(), "0".to_string()),
            ("WINEDLLOVERRIDES".to_string(), "winebus.sys=d".to_string()),
        ];
        if let Self::Proton { prefix, .. } = self {
            env.push((
                "WINEPREFIX".to_string(),
                prefix.to_string_lossy().into_owned(),
            ));
        }
        env
    }

    /// What to make the working directory.
    ///
    /// The folder the program is in. Wine resolves a path against the
    /// prefix's drive mappings, and a folder that is not mapped — `/tmp`, on
    /// a Proton prefix — comes back as "file not found" with the file plainly
    /// there.
    pub fn working_dir(exe: &Path) -> Option<PathBuf> {
        exe.parent().map(Path::to_path_buf)
    }
}

/// How to start the bridge for one game, best answer first.
pub fn how_to_launch(app_id: u32) -> Launch {
    match prefix_for(app_id) {
        Some(prefix) => match wine_for(&prefix) {
            Some(wine) => Launch::Proton { wine, prefix },
            None => Launch::Protontricks { app_id },
        },
        None => Launch::Protontricks { app_id },
    }
}

/// Whether this process is inside a Flatpak sandbox.
///
/// **Worth saying out loud, because it is the trap.** A Flatpak has its own
/// `/dev/shm`, so a bridge started from inside one publishes where nothing
/// outside can see it — and every symptom is "it says it worked and nothing
/// happens". Both signals are checked because a runtime may set one and not
/// the other.
pub fn inside_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wineshm-launch-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn every_layout_steam_ships_in_is_looked_at() {
        let home = Path::new("/home/someone");
        let roots = steam_roots_under(home);
        let shown: Vec<String> = roots.iter().map(|p| p.display().to_string()).collect();

        for expected in [
            "/home/someone/.steam/steam",
            "/home/someone/.local/share/Steam",
            "/home/someone/.var/app/com.valvesoftware.Steam/.local/share/Steam",
            "/home/someone/.var/app/com.valvesoftware.Steam/data/Steam",
            "/home/someone/snap/steam/common/.local/share/Steam",
        ] {
            assert!(shown.iter().any(|p| p == expected), "missing {expected}");
        }
    }

    #[test]
    fn a_library_file_gives_up_its_paths() {
        let vdf = r#"
"libraryfolders"
{
    "0"
    {
        "path"      "/home/someone/.local/share/Steam"
        "label"     ""
    }
    "1"
    {
        "path"      "/run/media/deck/SDCARD/steamlibrary"
    }
}
"#;
        assert_eq!(
            library_paths(vdf),
            vec![
                PathBuf::from("/home/someone/.local/share/Steam"),
                PathBuf::from("/run/media/deck/SDCARD/steamlibrary"),
            ]
        );
    }

    /// The Steam Deck's SD card is the case this matters for.
    #[test]
    fn a_library_on_another_disk_is_found() {
        let dir = scratch("second-library");
        let root = dir.join("Steam");
        let card = dir.join("sdcard");
        std::fs::create_dir_all(root.join("steamapps")).expect("root");
        std::fs::create_dir_all(prefix_in(&card, 244210)).expect("prefix");
        std::fs::write(
            root.join("steamapps").join("libraryfolders.vdf"),
            format!("\"path\"\t\t\"{}\"\n", card.display()),
        )
        .expect("vdf");

        let found = libraries(&[root]);
        assert!(found.contains(&card), "{found:?}");
        assert!(prefix_in(&card, 244210).is_dir());
    }

    #[test]
    fn a_library_file_that_is_not_there_is_not_an_error() {
        assert_eq!(libraries(&[PathBuf::from("/not/here")]).len(), 1);
    }

    #[test]
    fn junk_in_a_library_file_yields_no_paths_rather_than_nonsense() {
        assert!(library_paths("not a vdf at all").is_empty());
        assert!(library_paths("\"path\"").is_empty());
        assert!(library_paths("\"path\"  \"unterminated").is_empty());
        assert!(library_paths("").is_empty());
    }

    #[test]
    fn the_prefix_path_is_where_steam_puts_it() {
        assert_eq!(
            prefix_in(Path::new("/lib"), 244210),
            Path::new("/lib/steamapps/compatdata/244210/pfx")
        );
    }

    #[test]
    fn the_proton_that_built_a_prefix_is_read_out_of_it() {
        let info = "9.0-203\n/steam/common/Proton 9.0/files/share/fonts/\n/steam/lib/\n";
        assert_eq!(
            wine_from_config_info(info),
            Some(PathBuf::from("/steam/common/Proton 9.0/files/bin/wine"))
        );
    }

    #[test]
    fn a_config_info_that_says_nothing_useful_is_not_guessed_at() {
        assert_eq!(wine_from_config_info(""), None);
        assert_eq!(wine_from_config_info("9.0-203\n"), None);
        assert_eq!(wine_from_config_info("9.0-203\n/\n"), None);
    }

    #[test]
    fn a_prefix_whose_proton_has_been_deleted_falls_back() {
        let dir = scratch("gone-proton");
        let compat = dir.join("steamapps").join("compatdata").join("244210");
        std::fs::create_dir_all(compat.join("pfx")).expect("pfx");
        std::fs::write(
            compat.join("config_info"),
            "9.0-203\n/nowhere/files/share/fonts/\n",
        )
        .expect("config_info");

        assert_eq!(wine_for(&compat.join("pfx")), None);
    }

    #[test]
    fn a_prefix_with_its_proton_still_there_is_used() {
        let dir = scratch("live-proton");
        let files = dir.join("Proton").join("files");
        std::fs::create_dir_all(files.join("bin")).expect("bin");
        std::fs::write(files.join("bin").join("wine"), b"#!/bin/true").expect("wine");

        let compat = dir.join("steamapps").join("compatdata").join("244210");
        std::fs::create_dir_all(compat.join("pfx")).expect("pfx");
        std::fs::write(
            compat.join("config_info"),
            format!("9.0-203\n{}/share/fonts/\n", files.display()),
        )
        .expect("config_info");

        assert_eq!(
            wine_for(&compat.join("pfx")),
            Some(files.join("bin").join("wine"))
        );
    }

    #[test]
    fn launching_through_proton_sets_the_prefix_and_runs_the_exe() {
        let launch = Launch::Proton {
            wine: PathBuf::from("/steam/Proton/files/bin/wine"),
            prefix: PathBuf::from("/steam/compatdata/244210/pfx"),
        };
        let exe = Path::new("/home/someone/pro-engineer/wineshm.exe");
        let (program, args) = launch.command(exe);

        assert_eq!(program, Path::new("/steam/Proton/files/bin/wine"));
        assert_eq!(args, vec![exe.to_string_lossy().into_owned()]);
        assert!(
            launch
                .env()
                .iter()
                .any(|(k, v)| k == "WINEPREFIX" && v == "/steam/compatdata/244210/pfx"),
            "{:?}",
            launch.env()
        );
        assert_eq!(
            Launch::working_dir(exe).as_deref(),
            Some(Path::new("/home/someone/pro-engineer"))
        );
    }

    #[test]
    fn falling_back_speaks_protontricks_command_line() {
        let launch = Launch::Protontricks { app_id: 244210 };
        let exe = Path::new("/somewhere/wineshm.exe");
        let (program, args) = launch.command(exe);

        assert_eq!(program, Path::new("protontricks-launch"));
        assert_eq!(
            args,
            vec![
                "--appid".to_string(),
                "244210".to_string(),
                exe.to_string_lossy().into_owned()
            ]
        );
        assert!(
            !launch.env().iter().any(|(k, _)| k == "WINEPREFIX"),
            "protontricks finds the prefix itself"
        );
    }

    /// The two that stop winedevice spinning a core are set either way.
    #[test]
    fn the_wine_workarounds_are_set_on_both_routes() {
        for launch in [
            Launch::Protontricks { app_id: 1 },
            Launch::Proton {
                wine: PathBuf::from("/w"),
                prefix: PathBuf::from("/p"),
            },
        ] {
            let env = launch.env();
            assert!(env.iter().any(|(k, _)| k == "DBUS_FATAL_WARNINGS"));
            assert!(env.iter().any(|(k, _)| k == "WINEDLLOVERRIDES"));
        }
    }

    #[test]
    fn a_program_at_the_root_has_no_folder_to_sit_in() {
        assert_eq!(Launch::working_dir(Path::new("/")), None);
    }
}
