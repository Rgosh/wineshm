//! A game written down in a file, so that adding one is not a fork.
//!
//! **The preset list cannot be the only way in.** It has two games in it, and
//! every other program that publishes a named block — a simulator nobody here
//! owns, an instrument panel, somebody's own application — is somebody else's
//! problem to describe. Two ways to describe one exist already and neither is
//! good enough on its own: a row of `--page` flags is unmemorable and
//! unshareable, and a patch adding a preset means this crate has to know about
//! a program its author cannot run.
//!
//! So: a file. It says what a preset says, it lives next to whatever needs it,
//! and it can be posted in a forum thread and pasted by the next person.
//!
//! # The format
//!
//! ```text
//! # Assetto Corsa. Sizes are Competizione's, which are the larger.
//! acpmf_physics:2048
//! acpmf_graphics:2048
//! acpmf_static:2048
//! acpmf_crewchief:15660
//!
//! heartbeat acpmf_physics
//! ```
//!
//! Blank lines and everything after a `#` are ignored. A bare `NAME:SIZE` is a
//! page, in exactly the syntax `--page` takes, so what somebody learns at the
//! command line transfers. `heartbeat NAME` names the block that changes while
//! the program is alive — see [`crate::watchdog`] — and is the one piece of
//! knowledge a page list does not carry by itself.
//!
//! # Why so little
//!
//! Every directive is one more thing that can be spelled wrong in a file
//! nobody validates until the moment they are trying to drive. The pacing, the
//! directory and the rest stay on the command line, where a mistake is
//! answered immediately.

use crate::page::Page;
use core::fmt;

/// A game, read out of a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipe {
    /// The pages to publish.
    pub pages: Vec<Page>,
    /// The block that proves the program is alive, if it says.
    pub heartbeat: Option<String>,
}

/// Why a recipe could not be read, and which line was at fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRecipe {
    /// The line number, counting from one, as an editor shows it.
    pub line: usize,
    /// What was wrong.
    pub why: String,
}

impl fmt::Display for BadRecipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.why)
    }
}

impl std::error::Error for BadRecipe {}

/// Read a recipe.
///
/// The line number travels with every complaint, because the whole point of
/// putting this in a file is that somebody edits the file — and "that is not a
/// size" is worth nothing without it.
pub fn parse(text: &str) -> Result<Recipe, BadRecipe> {
    let mut recipe = Recipe::default();

    for (at, raw) in text.lines().enumerate() {
        let line = at + 1;
        let content = match raw.split_once('#') {
            Some((before, _)) => before,
            None => raw,
        }
        .trim();
        if content.is_empty() {
            continue;
        }

        // The whole word, not a prefix of a name: `heartbeatish:16` is a page
        // somebody is entitled to call that, and swallowing it as a directive
        // would publish nothing and complain about a size that was never there.
        if let Some(rest) = content
            .strip_prefix("heartbeat")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            let name = rest.trim();
            if name.is_empty() {
                return Err(BadRecipe {
                    line,
                    why: "heartbeat needs the name of a block".to_string(),
                });
            }
            if recipe.heartbeat.is_some() {
                return Err(BadRecipe {
                    line,
                    why: "a second heartbeat — only one block can prove the program is alive"
                        .to_string(),
                });
            }
            recipe.heartbeat = Some(name.to_string());
            continue;
        }

        let page = Page::parse(content).map_err(|why| BadRecipe {
            line,
            why: format!("{content}: {why}"),
        })?;
        if recipe.pages.iter().any(|had| had.name == page.name) {
            return Err(BadRecipe {
                line,
                why: format!("{} was already asked for", page.name),
            });
        }
        recipe.pages.push(page);
    }

    // A heartbeat naming a block the file does not publish watches nothing,
    // for ever, and silently — the same check the command line makes, made
    // here so that a file is wrong when it is read rather than when it runs.
    if let Some(name) = recipe.heartbeat.as_deref()
        && !recipe.pages.iter().any(|page| page.name == name)
    {
        return Err(BadRecipe {
            line: text.lines().count().max(1),
            why: format!("heartbeat {name} is not one of the pages this file publishes"),
        });
    }

    if recipe.pages.is_empty() {
        return Err(BadRecipe {
            line: text.lines().count().max(1),
            why: "no pages in this file".to_string(),
        });
    }

    Ok(recipe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_in_the_documentation_parses() {
        let recipe = parse(
            "# Assetto Corsa\n\
             acpmf_physics:2048\n\
             acpmf_graphics:2048\n\
             acpmf_static:2048\n\
             acpmf_crewchief:15660\n\
             \n\
             heartbeat acpmf_physics\n",
        )
        .expect("parses");
        assert_eq!(recipe.pages.len(), 4);
        assert_eq!(recipe.heartbeat.as_deref(), Some("acpmf_physics"));
        assert_eq!(recipe.pages[3].bytes, 15660);
    }

    /// The same list the `--page` flag takes, so what somebody learns at the
    /// command line is what they write in the file.
    #[test]
    fn a_size_may_carry_the_same_suffixes_the_flag_takes() {
        let recipe = parse("a:16K\nb:1M\n").expect("parses");
        assert_eq!(recipe.pages[0].bytes, 16 * 1024);
        assert_eq!(recipe.pages[1].bytes, 1024 * 1024);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored_wherever_they_are() {
        let recipe = parse(
            "\n\
             # a whole-line comment\n\
             a:16   # and one after a page\n\
             \n   \n\
             b:32\n",
        )
        .expect("parses");
        assert_eq!(recipe.pages.len(), 2);
        assert_eq!(recipe.pages[1].name, "b");
    }

    /// The line number is the reason the file is worth having: an editor shows
    /// them, and "that is not a size" alone sends somebody hunting.
    #[test]
    fn a_bad_line_says_which_line_it_was() {
        let bad = parse("a:16\nb:16\nc:nonsense\n").expect_err("refused");
        assert_eq!(bad.line, 3);
        assert!(bad.why.contains("nonsense"), "{bad}");
        assert!(format!("{bad}").starts_with("line 3:"), "{bad}");
    }

    #[test]
    fn a_page_asked_for_twice_is_refused_with_its_line() {
        let bad = parse("a:16\nb:16\na:32\n").expect_err("refused");
        assert_eq!(bad.line, 3);
        assert!(bad.why.contains('a'), "{bad}");
    }

    /// The failure this check exists for: a heartbeat with a typo watches a
    /// block that does not exist, for ever, and says nothing.
    #[test]
    fn a_heartbeat_that_names_no_published_block_is_refused() {
        let bad = parse("a:16\nheartbeat b\n").expect_err("refused");
        assert!(bad.why.contains('b'), "{bad}");
    }

    #[test]
    fn a_heartbeat_with_nothing_after_it_is_refused() {
        let bad = parse("a:16\nheartbeat\n").expect_err("refused");
        assert!(bad.why.contains("heartbeat"), "{bad}");
    }

    /// Two of them is not a richer configuration, it is a file whose author
    /// believes something this cannot do.
    #[test]
    fn two_heartbeats_are_refused_rather_than_one_winning() {
        let bad = parse("a:16\nb:16\nheartbeat a\nheartbeat b\n").expect_err("refused");
        assert_eq!(bad.line, 4);
    }

    #[test]
    fn a_file_with_nothing_in_it_is_refused() {
        for empty in ["", "\n\n\n", "# only a comment\n"] {
            let bad = parse(empty).expect_err("refused");
            assert!(bad.why.contains("no pages"), "{bad}");
        }
    }

    /// A name with a colon in it is legal in Win32, and the size is what comes
    /// after the *last* colon. The file has to agree with the flag.
    #[test]
    fn a_name_containing_a_colon_survives() {
        let recipe = parse("Local:thing:2048\n").expect("parses");
        assert_eq!(recipe.pages[0].name, "Local:thing");
        assert_eq!(recipe.pages[0].bytes, 2048);
    }

    /// A page whose name begins with the word does not become a directive.
    #[test]
    fn a_page_named_after_the_directive_is_still_a_page() {
        let recipe = parse("heartbeatish:16\nheartbeat heartbeatish\n").expect("parses");
        assert_eq!(recipe.pages.len(), 1);
        assert_eq!(recipe.pages[0].name, "heartbeatish");
        assert_eq!(recipe.heartbeat.as_deref(), Some("heartbeatish"));
    }

    #[test]
    fn carriage_returns_from_a_windows_editor_do_not_leak_into_a_name() {
        let recipe = parse("a:16\r\nb:32\r\nheartbeat a\r\n").expect("parses");
        assert_eq!(recipe.pages[0].name, "a");
        assert_eq!(recipe.heartbeat.as_deref(), Some("a"));
    }
}
