//! Working with the configuration file directly.
//!
//! The file lives wherever the platform says application data belongs, which on
//! macOS is a path containing a space and on every platform is somewhere you
//! would not guess. Rather than move the file somewhere less correct, these
//! commands remove any need to type the path at all.

use std::env;
use std::process::Command;

use color_eyre::eyre::{Context, Result, bail};

use crate::config::{Config, Paths};
use crate::ui::theme::{self, Level};

use super::say;

/// `tgfwd config path`
///
/// Printed on stdout, alone, so it composes: `$EDITOR "$(tgfwd config path)"`.
///
/// The only `println!` in the crate, and the reason stdout is off limits
/// everywhere else: anything else printed there would end up inside the
/// substitution alongside the path.
#[allow(clippy::disallowed_macros, reason = "this is the composable output")]
pub fn path(paths: &Paths) {
    println!("{}", paths.config_file().display());
}

/// `tgfwd config edit`
///
/// Opens the file in the user's editor, creating a commented template first if
/// it does not exist yet, and re-checks the result so a typo is caught here
/// rather than at the next start.
pub fn edit(paths: &Paths) -> Result<()> {
    paths.ensure_dirs()?;
    let file = paths.config_file();

    if !file.exists() {
        // Writing the default gives the user a commented starting point instead
        // of an empty buffer.
        Config::default().save(paths)?;
        say(Level::Info, "created a new configuration file");
    }

    let editor = editor_command();
    let (program, args) = editor.split_first().expect("editor command is non-empty");

    let status = Command::new(program)
        .args(args)
        .arg(&file)
        .status()
        .wrap_err_with(|| format!("could not start editor '{program}'"))?;

    if !status.success() {
        bail!("editor '{program}' exited with an error");
    }

    // Re-read it: a config that will not parse, discovered now, is a much better
    // experience than one discovered when forwarding is supposed to start.
    match Config::load(paths) {
        Ok(config) => match config.validate() {
            Ok(()) => {
                say(Level::Success, "configuration is valid");
                Ok(())
            }
            Err(error) => {
                say(Level::Error, "the file was saved, but it has problems:");
                Err(error)
            }
        },
        Err(error) => {
            say(Level::Error, "the file was saved, but it no longer parses:");
            Err(error)
        }
    }
}

/// Work out which editor to launch.
fn editor_command() -> Vec<String> {
    let configured = env::var("VISUAL")
        .or_else(|_| env::var("EDITOR"))
        .unwrap_or_default();

    if let Some(command) = parse_editor(&configured) {
        return command;
    }

    // No preference set. `vi` exists on every Unix; on Windows, `notepad` does.
    let fallback = default_editor();
    say(
        Level::Info,
        format!(
            "{} is not set, falling back to {}",
            theme::bold("EDITOR"),
            theme::accent(fallback)
        ),
    );
    vec![fallback.to_owned()]
}

/// Split an editor setting into a program and its arguments.
///
/// `VISUAL` and `EDITOR` may hold arguments as well as a program name
/// (`code --wait` is common), so the value cannot be used as an executable name
/// as-is. Returns `None` when nothing is configured.
///
/// Kept separate from [`editor_command`] so it is testable without mutating
/// process-global environment state, which `unsafe_code = "forbid"` rules out.
fn parse_editor(configured: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = configured.split_whitespace().map(str::to_owned).collect();

    if parts.is_empty() { None } else { Some(parts) }
}

/// The editor to use when the user has expressed no preference.
const fn default_editor() -> &'static str {
    if cfg!(windows) { "notepad" } else { "vi" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_editor_with_arguments_is_split_correctly() {
        // `code --wait` must not be looked up as a single executable name.
        assert_eq!(
            parse_editor("code --wait"),
            Some(vec!["code".to_owned(), "--wait".to_owned()])
        );
    }

    #[test]
    fn a_plain_editor_name_is_a_single_word() {
        assert_eq!(parse_editor("nvim"), Some(vec!["nvim".to_owned()]));
    }

    #[test]
    fn an_unset_editor_falls_through_to_the_default() {
        assert_eq!(parse_editor(""), None);
        assert_eq!(parse_editor("   "), None);
        assert_ne!(default_editor(), "");
    }
}
