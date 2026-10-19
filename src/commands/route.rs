//! Creating and managing routes.
//!
//! Every chat here is chosen from the account's own dialog list. Nothing in this
//! module accepts a hand-typed chat ID, because a mistyped one fails silently at
//! delivery time rather than loudly at configuration time.

use color_eyre::eyre::{Result, bail};

use crate::config::{Config, DeliveryMode, Filter, MediaKind, Paths, PeerLink, Route};
use crate::telegram::dialogs::DialogEntry;
use crate::ui::prompts;
use crate::ui::theme::{self, Level};

use super::say;

impl From<&DialogEntry> for PeerLink {
    fn from(entry: &DialogEntry) -> Self {
        Self {
            id: entry.id,
            title: entry.title.clone(),
            username: entry.username.clone(),
        }
    }
}

/// `tgfwd route add`
pub async fn add(paths: &Paths) -> Result<()> {
    let mut config = super::load_config_with_credentials(paths).await?;
    let (connection, chats) = super::fetch_chats(paths, &config).await?;

    let sources =
        prompts::pick_chats("Which chats should be watched?", chats.clone(), &[], false).await?;

    if sources.is_empty() {
        bail!("a route needs at least one source");
    }

    // Offering a chat as its own target is the single easiest way to create an
    // infinite loop, so it is not offered at all.
    let source_ids: Vec<i64> = sources.iter().map(|entry| entry.id).collect();
    let target_candidates: Vec<DialogEntry> = chats
        .into_iter()
        .filter(|chat| !source_ids.contains(&chat.id))
        .collect();

    let targets = prompts::pick_chats(
        "Where should they be forwarded to?",
        target_candidates,
        &[],
        true,
    )
    .await?;

    if targets.is_empty() {
        bail!("a route needs at least one target");
    }

    warn_about_unwritable(&targets);

    let mode = ask_for_mode().await?;
    let filter = ask_for_filter().await?;
    let id = auto_id(&config, &sources);

    config.routes.push(Route {
        id: id.clone(),
        enabled: true,
        sources: sources.iter().map(PeerLink::from).collect(),
        targets: targets.iter().map(PeerLink::from).collect(),
        mode: override_mode(mode, &config),
        filter,
    });

    config.validate()?;
    config.save(paths)?;

    eprintln!();
    say(
        Level::Success,
        format!(
            "route {} created: {} source(s) {} {} target(s)",
            theme::accent(&id),
            sources.len(),
            theme::arrow(),
            targets.len()
        ),
    );
    say(
        Level::Info,
        format!("start forwarding with {}", theme::accent("tgfwd start")),
    );

    connection.shutdown().await
}

/// Record a delivery mode only when it differs from the configured default.
///
/// Writing the chosen mode unconditionally would pin every route to whatever it
/// was created with and make `defaults.mode` a setting that never applies to
/// anything, which is not what the file says it does.
fn override_mode(chosen: DeliveryMode, config: &Config) -> Option<DeliveryMode> {
    (chosen != config.defaults.mode).then_some(chosen)
}

/// Point out targets the account probably cannot post into.
fn warn_about_unwritable(targets: &[DialogEntry]) {
    let blocked: Vec<&DialogEntry> = targets
        .iter()
        .filter(|target| !target.likely_writable)
        .collect();

    if blocked.is_empty() {
        return;
    }

    say(Level::Warn, "you do not appear to have posting rights in:");
    for target in blocked {
        eprintln!("    - {} ({})", target.title, target.id);
    }
    eprintln!(
        "    {}",
        theme::dim("delivery there will fail until you are given permission")
    );
}

/// Derive a unique identifier for a new route, without asking anyone.
///
/// Routes need a stable handle so logs and shell scripts can name
/// one, but that is a machine's requirement, not the user's. Making someone
/// invent a name is a decision they did not ask to make, about a string they
/// will not remember. So it is derived from the first source chat, and collisions
/// get a numeric suffix.
fn auto_id(config: &Config, sources: &[DialogEntry]) -> String {
    let base = slugify(&sources[0].title);

    if config.route(&base).is_none() {
        return base;
    }

    // With N existing routes, at most N of the candidates can be taken, so a
    // range of N + 2 is guaranteed to contain a free one.
    let limit = config.routes.len() + 2;
    (2..=limit)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| config.route(candidate).is_none())
        .expect("the range is larger than the number of routes")
}

/// Summarise one side of a route for a human reading a list.
///
/// People recognise a route by what it moves, not by its identifier, so this is
/// what the pickers show.
fn summarise(peers: &[PeerLink]) -> String {
    match peers {
        [] => "nothing".to_owned(),
        [one] => one.label(),
        [first, rest @ ..] => format!("{} +{} more", first.label(), rest.len()),
    }
}

/// Render a route as the flow it describes: `source → target`.
fn describe_route(route: &Route) -> String {
    format!(
        "{} {} {}",
        summarise(&route.sources),
        theme::arrow(),
        summarise(&route.targets)
    )
}

/// Turn a chat title into a usable identifier.
///
/// Non-ASCII titles are common, and transliterating them is worse than leaving
/// them alone, so anything that is not obviously separable is kept as-is.
fn slugify(title: &str) -> String {
    let slug: String = title
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    let slug = slug.trim_matches('-').to_owned();
    let collapsed = slug
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if collapsed.is_empty() {
        "route".to_owned()
    } else {
        collapsed.chars().take(32).collect()
    }
}

/// Ask which delivery mode this route should use.
async fn ask_for_mode() -> Result<DeliveryMode> {
    ask_for_mode_starting_at(DeliveryMode::Auto).await
}

/// Ask for a delivery mode with `current` under the cursor.
async fn ask_for_mode_starting_at(current: DeliveryMode) -> Result<DeliveryMode> {
    let start = match current {
        DeliveryMode::Auto => 0,
        DeliveryMode::Copy => 1,
        DeliveryMode::Forward => 2,
    };

    prompts::select_starting_at(
        "How should messages be delivered?",
        vec![
            (
                "auto — keep attribution, fall back to a copy if the source is deleted".to_owned(),
                DeliveryMode::Auto,
            ),
            (
                "copy — always re-send as your own message, no attribution".to_owned(),
                DeliveryMode::Copy,
            ),
            (
                "forward — native forward only, fails if the source is gone".to_owned(),
                DeliveryMode::Forward,
            ),
        ],
        start,
    )
    .await
}

/// Ask whether a brand-new route wants a filter, and build it if so.
async fn ask_for_filter() -> Result<Filter> {
    if prompts::confirm("Add a content filter?", false).await? {
        edit_filter(&Filter::default()).await
    } else {
        Ok(Filter::default())
    }
}

/// Revise an existing filter, offering every current value back.
///
/// Nothing here starts from blank when there is already an answer in the file:
/// a filter is edited by adjusting what it says, not by remembering and retyping
/// it. Clearing one is an explicit choice, not the default outcome of pressing
/// enter through the questions.
async fn edit_filter(current: &Filter) -> Result<Filter> {
    if !current.is_empty()
        && prompts::confirm(
            &format!("Remove the filter entirely? ({})", describe_filter(current)),
            false,
        )
        .await?
    {
        return Ok(Filter::default());
    }

    let mut filter = Filter::default();

    let include = prompts::edit_text(
        "Only forward messages containing (comma-separated, blank for all)",
        Some("matched case-insensitively anywhere in the text"),
        current.include.join(", "),
    )
    .await?;
    filter.include = split_keywords(&include);

    let exclude = prompts::edit_text(
        "Never forward messages containing (comma-separated)",
        None,
        current.exclude.join(", "),
    )
    .await?;
    filter.exclude = split_keywords(&exclude);

    if prompts::confirm(
        "Restrict to certain media kinds?",
        !current.kinds.is_empty(),
    )
    .await?
    {
        let options: Vec<(String, MediaKind)> = MediaKind::ALL
            .iter()
            .map(|kind| (kind.to_string(), *kind))
            .collect();
        let preselected: Vec<usize> = MediaKind::ALL
            .iter()
            .enumerate()
            .filter(|(_, kind)| current.kinds.contains(kind))
            .map(|(index, _)| index)
            .collect();

        filter.kinds = prompts::select_many("Which kinds?", options, &preselected)
            .await?
            .into_iter()
            .collect();
    }

    filter.require_media =
        prompts::confirm("Skip messages with no media?", current.require_media).await?;
    filter.skip_forwarded = prompts::confirm(
        "Skip messages that are themselves forwards?",
        current.skip_forwarded,
    )
    .await?;

    Ok(filter)
}

/// Split a comma-separated list into trimmed, non-empty keywords.
fn split_keywords(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `tgfwd route list`
pub fn list(paths: &Paths) -> Result<()> {
    let config = Config::load(paths)?;

    if config.routes.is_empty() {
        say(
            Level::Info,
            format!("no routes yet — run {}", theme::accent("tgfwd route add")),
        );
        return Ok(());
    }

    print_routes(&config);
    Ok(())
}

/// Render the configured routes.
pub fn print_routes(config: &Config) {
    for route in &config.routes {
        let state = if route.enabled {
            theme::Level::Success.paint("enabled")
        } else {
            theme::dim("disabled")
        };

        eprintln!();
        eprintln!(
            "  {} {}  {}  {}",
            theme::bold(&route.id),
            theme::dim(&format!("({})", route.mode(&config.defaults))),
            state,
            if route.filter.is_empty() {
                String::new()
            } else {
                theme::dim("filtered")
            }
        );

        for source in &route.sources {
            eprintln!("    {} {}", theme::dim("from"), source);
        }
        for target in &route.targets {
            eprintln!("    {}   {}", theme::dim(theme::arrow()), target);
        }
    }
    eprintln!();
}

/// What part of a route is being changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aspect {
    Sources,
    Targets,
    Mode,
    Filter,
}

/// Summarise a filter for the edit menu.
fn describe_filter(filter: &Filter) -> String {
    if filter.is_empty() {
        return "none".to_owned();
    }

    let mut parts = Vec::new();
    if !filter.include.is_empty() {
        parts.push(format!("{} required", filter.include.len()));
    }
    if !filter.exclude.is_empty() {
        parts.push(format!("{} blocked", filter.exclude.len()));
    }
    if !filter.kinds.is_empty() {
        parts.push(format!("{} kind(s)", filter.kinds.len()));
    }
    if filter.require_media {
        parts.push("media only".to_owned());
    }
    if filter.skip_forwarded {
        parts.push("no forwards".to_owned());
    }
    parts.join(", ")
}

/// `tgfwd route edit`
///
/// Asks which part to change rather than walking through everything, so keeping
/// the parts you are happy with costs nothing. Only the chat pickers need a
/// connection, so changing a mode or a filter stays offline and instant.
pub async fn edit(paths: &Paths, route: Option<String>) -> Result<()> {
    let config = Config::load(paths)?;
    let id = resolve_route_id(&config, route, Selectable::Any).await?;

    let Some(existing) = config.route(&id).cloned() else {
        bail!("{}", unknown_route(&config, &id));
    };

    let current_mode = existing.mode(&config.defaults);
    let aspect = prompts::select(
        // What it moves, not what it is called. With one route configured the
        // picker above is skipped entirely, so this line is the only thing
        // telling the user which route they are about to change.
        format!(
            "Editing {} — what would you like to change?",
            describe_route(&existing)
        ),
        vec![
            (
                format!("Sources        ({})", summarise(&existing.sources)),
                Aspect::Sources,
            ),
            (
                format!("Targets        ({})", summarise(&existing.targets)),
                Aspect::Targets,
            ),
            (format!("Delivery mode  ({current_mode})"), Aspect::Mode),
            (
                format!("Filter         ({})", describe_filter(&existing.filter)),
                Aspect::Filter,
            ),
        ],
    )
    .await?;

    // Only the two chat pickers need the dialog list, so the other two aspects
    // never ask for API credentials and never open a connection. Re-reading the
    // file here matters: prompting for credentials writes them to disk, and this
    // copy would otherwise overwrite them again on save.
    let mut config = match aspect {
        Aspect::Sources | Aspect::Targets => super::load_config_with_credentials(paths).await?,
        Aspect::Mode | Aspect::Filter => config,
    };

    let connection = match aspect {
        Aspect::Sources | Aspect::Targets => {
            let (connection, chats) = super::fetch_chats(paths, &config).await?;
            let picked = edit_chats(aspect, &existing, chats).await?;

            for route in &mut config.routes {
                if route.id == id {
                    match aspect {
                        Aspect::Sources => {
                            route.sources = picked.iter().map(PeerLink::from).collect();
                        }
                        _ => route.targets = picked.iter().map(PeerLink::from).collect(),
                    }
                }
            }
            Some(connection)
        }

        Aspect::Mode => {
            let mode = ask_for_mode_starting_at(current_mode).await?;
            let stored = override_mode(mode, &config);
            for route in &mut config.routes {
                if route.id == id {
                    route.mode = stored;
                }
            }
            None
        }

        Aspect::Filter => {
            let filter = edit_filter(&existing.filter).await?;
            for route in &mut config.routes {
                if route.id == id {
                    route.filter = filter.clone();
                }
            }
            None
        }
    };

    config.validate()?;
    config.save(paths)?;
    say(
        Level::Success,
        format!("route {} updated", theme::accent(&id)),
    );

    match connection {
        Some(connection) => connection.shutdown().await,
        None => Ok(()),
    }
}

/// Re-pick one side of a route, with the current selection already checked.
async fn edit_chats(
    aspect: Aspect,
    existing: &Route,
    chats: Vec<DialogEntry>,
) -> Result<Vec<DialogEntry>> {
    let (current, opposite, message, warn) = match aspect {
        Aspect::Sources => (
            &existing.sources,
            &existing.targets,
            "Which chats should be watched?",
            false,
        ),
        _ => (
            &existing.targets,
            &existing.sources,
            "Where should they be forwarded to?",
            true,
        ),
    };

    // A chat cannot be both ends of the same route, so the other side is not
    // offered at all.
    let excluded: Vec<i64> = opposite.iter().map(|peer| peer.id).collect();
    let candidates: Vec<DialogEntry> = chats
        .into_iter()
        .filter(|chat| !excluded.contains(&chat.id))
        .collect();

    let preselected: Vec<i64> = current.iter().map(|peer| peer.id).collect();
    let picked = prompts::pick_chats(message, candidates, &preselected, warn).await?;

    if picked.is_empty() {
        bail!(
            "a route needs at least one {}",
            if aspect == Aspect::Sources {
                "source"
            } else {
                "target"
            }
        );
    }

    if warn {
        warn_about_unwritable(&picked);
    }

    Ok(picked)
}

/// `tgfwd route remove`
pub async fn remove(paths: &Paths, route: Option<String>) -> Result<()> {
    let mut config = Config::load(paths)?;
    let id = resolve_route_id(&config, route, Selectable::Any).await?;

    let Some(doomed) = config.route(&id) else {
        bail!("{}", unknown_route(&config, &id));
    };

    // Naming what is about to be destroyed by what it moves, not by its
    // identifier. Deleting is the one action nobody can undo, and with a single
    // route configured the picker is skipped, so this prompt may be the first
    // and last chance to notice it is the wrong one.
    if !prompts::confirm(
        format!("Delete this route? {}", describe_route(doomed)),
        false,
    )
    .await?
    {
        say(Level::Info, "cancelled");
        return Ok(());
    }

    config.routes.retain(|route| route.id != id);
    config.save(paths)?;
    say(Level::Success, format!("route {id} deleted"));
    Ok(())
}

/// `tgfwd route enable` / `tgfwd route disable`
pub async fn set_enabled(paths: &Paths, route: Option<String>, enabled: bool) -> Result<()> {
    let mut config = Config::load(paths)?;

    // Only offer routes that would actually change: picking an already-enabled
    // route from an "enable" list is a dead end.
    let wanted = if enabled {
        Selectable::Disabled
    } else {
        Selectable::Enabled
    };
    let id = resolve_route_id(&config, route, wanted).await?;

    let Some(route) = config.routes.iter_mut().find(|route| route.id == id) else {
        bail!("{}", unknown_route(&config, &id));
    };

    if route.enabled == enabled {
        say(
            Level::Info,
            format!(
                "route {id} is already {}",
                if enabled { "enabled" } else { "disabled" }
            ),
        );
        return Ok(());
    }

    route.enabled = enabled;
    config.validate()?;
    config.save(paths)?;

    say(
        Level::Success,
        format!(
            "route {id} {}",
            if enabled { "enabled" } else { "disabled" }
        ),
    );
    Ok(())
}

/// `tgfwd route sync` — refresh stored chat titles.
pub async fn sync(paths: &Paths) -> Result<()> {
    // Having nothing to sync is knowable from the file, so it is settled before
    // the credential prompt rather than after it.
    if Config::load(paths)?.routes.is_empty() {
        say(Level::Info, "no routes to sync");
        return Ok(());
    }

    let mut config = super::load_config_with_credentials(paths).await?;
    let (connection, chats) = super::fetch_chats(paths, &config).await?;
    let mut renamed = 0;
    let mut missing = Vec::new();

    for route in &mut config.routes {
        for peer in route.sources.iter_mut().chain(route.targets.iter_mut()) {
            match chats.iter().find(|chat| chat.id == peer.id) {
                Some(chat) => {
                    if peer.title != chat.title || peer.username != chat.username {
                        peer.title = chat.title.clone();
                        peer.username = chat.username.clone();
                        renamed += 1;
                    }
                }
                None => missing.push(format!("{peer} (route '{}')", route.id)),
            }
        }
    }

    config.save(paths)?;

    say(Level::Success, format!("{renamed} chat label(s) refreshed"));
    if !missing.is_empty() {
        say(
            Level::Warn,
            "these chats are no longer in your dialog list:",
        );
        for chat in missing {
            eprintln!("    - {chat}");
        }
    }

    connection.shutdown().await
}

/// Which routes a picker should offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selectable {
    /// Any route, regardless of state.
    Any,
    /// Only routes that are currently on.
    Enabled,
    /// Only routes that are currently off.
    Disabled,
}

impl Selectable {
    fn accepts(self, enabled: bool) -> bool {
        match self {
            Self::Any => true,
            Self::Enabled => enabled,
            Self::Disabled => !enabled,
        }
    }

    /// What to say when nothing matches.
    fn nothing_to_show(self) -> &'static str {
        match self {
            Self::Any => "there are no routes yet — run `tgfwd route add`",
            Self::Enabled => "every route is already disabled",
            Self::Disabled => "every route is already enabled",
        }
    }
}

/// Build the "no such route" message, listing what does exist.
///
/// A bare "not found" leaves the user guessing at a name they chose themselves,
/// possibly weeks ago.
fn unknown_route(config: &Config, id: &str) -> String {
    if config.routes.is_empty() {
        return format!("no route named '{id}' — there are no routes yet");
    }

    let names: Vec<&str> = config
        .routes
        .iter()
        .map(|route| route.id.as_str())
        .collect();
    format!(
        "no route named '{id}'. Existing routes: {}",
        names.join(", ")
    )
}

/// The routes a picker should offer, as `(what it moves, its id)` pairs.
fn selectable_routes(config: &Config, selectable: Selectable) -> Vec<(String, String)> {
    config
        .routes
        .iter()
        .filter(|route| selectable.accepts(route.enabled))
        .map(|route| {
            let state = if route.enabled { "" } else { "  [disabled]" };
            (
                format!("{}{state}", describe_route(route)),
                route.id.clone(),
            )
        })
        .collect()
}

/// Resolve a route name, prompting when it was not given on the command line.
///
/// The prompt appears even when only one route could be meant. Skipping it would
/// save a keystroke and cost consistency: the same command would sometimes ask
/// and sometimes not, depending on how many routes happen to exist, and the
/// single-route case is precisely the one where nothing else on screen confirms
/// which route is about to be edited, disabled or deleted.
async fn resolve_route_id(
    config: &Config,
    id: Option<String>,
    selectable: Selectable,
) -> Result<String> {
    // A name given on the command line is an answer already; scripts and cron
    // jobs have no one to ask.
    if let Some(id) = id {
        return Ok(id);
    }

    // "every route is already enabled" would be a confusing thing to say to
    // someone who has no routes at all, so that case is reported on its own.
    if config.routes.is_empty() {
        bail!("{}", Selectable::Any.nothing_to_show());
    }

    let options = selectable_routes(config, selectable);
    if options.is_empty() {
        bail!("{}", selectable.nothing_to_show());
    }

    prompts::select("Which route?", options).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_routes_moving_the_same_chats_render_identically() {
        // Not a defect in the rendering: a route is described by what it moves,
        // and two routes moving the same chats under different filters or modes
        // are a legitimate configuration. It is why the picker in `ui::prompts`
        // resolves a choice by its position and never by its label — the labels
        // are not unique, so a lookup could only ever find the first.
        let mut config = config_with(&[("urgent-only", true), ("everything", true)]);
        for route in &mut config.routes {
            route.sources = vec![PeerLink {
                id: -1001,
                title: "Breaking News".to_owned(),
                username: None,
            }];
            route.targets = vec![PeerLink {
                id: -2001,
                title: "Team".to_owned(),
                username: None,
            }];
        }

        let options = selectable_routes(&config, Selectable::Any);
        assert_eq!(options[0].0, options[1].0, "labels are expected to collide");
        assert_ne!(options[0].1, options[1].1, "the ids behind them must not");
    }

    #[test]
    fn slugify_makes_an_ascii_identifier() {
        assert_eq!(slugify("Tech News Daily"), "tech-news-daily");
        assert_eq!(slugify("  spaced  out  "), "spaced-out");
        assert_eq!(slugify("Rust!!! Weekly"), "rust-weekly");
    }

    #[test]
    fn slugify_keeps_chinese_titles_intact() {
        // Transliterating would produce something the user does not recognise;
        // the characters are valid in an identifier here.
        assert_eq!(slugify("台灣科技新聞"), "台灣科技新聞");
    }

    #[test]
    fn slugify_always_produces_something_usable() {
        assert_eq!(slugify("!!!"), "route");
        assert_eq!(slugify(""), "route");
    }

    #[test]
    fn slugify_caps_the_length() {
        assert!(slugify(&"word ".repeat(50)).chars().count() <= 32);
    }

    fn config_with(ids: &[(&str, bool)]) -> Config {
        Config {
            routes: ids
                .iter()
                .map(|(id, enabled)| Route {
                    id: (*id).to_owned(),
                    enabled: *enabled,
                    sources: vec![PeerLink {
                        id: -1,
                        title: "s".to_owned(),
                        username: None,
                    }],
                    targets: vec![PeerLink {
                        id: -2,
                        title: "t".to_owned(),
                        username: None,
                    }],
                    mode: None,
                    filter: Filter::default(),
                })
                .collect(),
            ..Config::default()
        }
    }

    fn dialog(title: &str) -> DialogEntry {
        DialogEntry {
            id: -1,
            title: title.to_owned(),
            username: None,
            kind: crate::telegram::dialogs::ChatKind::Channel,
            likely_writable: true,
        }
    }

    #[test]
    fn a_new_route_names_itself_after_its_source() {
        // The user is never asked; this is the whole point.
        let config = config_with(&[]);
        assert_eq!(
            auto_id(&config, &[dialog("Breaking News")]),
            "breaking-news"
        );
    }

    #[test]
    fn a_second_route_from_the_same_source_gets_a_suffix() {
        let config = config_with(&[("breaking-news", true)]);
        assert_eq!(
            auto_id(&config, &[dialog("Breaking News")]),
            "breaking-news-2"
        );
    }

    #[test]
    fn auto_naming_keeps_finding_free_names() {
        let config = config_with(&[
            ("breaking-news", true),
            ("breaking-news-2", true),
            ("breaking-news-3", true),
        ]);
        assert_eq!(
            auto_id(&config, &[dialog("Breaking News")]),
            "breaking-news-4"
        );
    }

    #[test]
    fn a_route_is_described_by_what_it_moves() {
        // People recognise "Breaking News → Team channel", not "breaking-news".
        let route = &config_with(&[("anything", true)]).routes[0];
        let described = describe_route(route);

        assert!(described.contains('s'), "{described}");
        assert!(described.contains(theme::arrow()), "{described}");
    }

    #[test]
    fn several_chats_collapse_into_a_count() {
        let peers: Vec<PeerLink> = ["A", "B", "C"]
            .iter()
            .map(|title| PeerLink {
                id: -1,
                title: (*title).to_owned(),
                username: None,
            })
            .collect();

        assert_eq!(summarise(&peers), "A +2 more");
        assert_eq!(summarise(&peers[..1]), "A");
        assert_eq!(summarise(&[]), "nothing");
    }

    #[test]
    fn enable_only_offers_routes_that_are_off() {
        assert!(Selectable::Disabled.accepts(false));
        assert!(!Selectable::Disabled.accepts(true));
        assert!(Selectable::Enabled.accepts(true));
        assert!(!Selectable::Enabled.accepts(false));
        assert!(Selectable::Any.accepts(true) && Selectable::Any.accepts(false));
    }

    #[test]
    fn a_route_is_named_to_the_user_by_what_it_moves() {
        // The identifier is for logs and shell scripts. When a
        // route is put in front of a person — especially at the two points where
        // the picker is skipped because there is only one — it has to be named
        // by what it moves, or the prompt says nothing they can act on.
        let route = Route {
            id: "self".to_owned(),
            enabled: true,
            sources: vec![PeerLink {
                id: -1001,
                title: "Breaking News".to_owned(),
                username: None,
            }],
            targets: vec![PeerLink {
                id: -2001,
                title: "Team channel".to_owned(),
                username: None,
            }],
            mode: None,
            filter: Filter::default(),
        };

        let described = describe_route(&route);
        assert!(described.contains("Breaking News"), "{described}");
        assert!(described.contains("Team channel"), "{described}");
        assert!(
            !described.contains("self"),
            "the identifier is not what a person recognises: {described}"
        );
    }

    #[test]
    fn a_lone_route_is_still_offered_for_selection() {
        // Deliberately not short-circuited. The same command must behave the
        // same way whether one route exists or ten, and with one route this
        // prompt is the only thing confirming which route is about to be
        // edited, disabled or deleted.
        let config = config_with(&[("only-one", false), ("already-on", true)]);
        let offered = selectable_routes(&config, Selectable::Disabled);

        assert_eq!(offered.len(), 1, "one candidate is still a choice to make");
        assert_eq!(offered[0].1, "only-one");
    }

    #[test]
    fn a_picker_offers_only_the_routes_that_would_change() {
        let config = config_with(&[("on-a", true), ("off-b", false), ("on-c", true)]);

        let to_enable: Vec<String> = selectable_routes(&config, Selectable::Disabled)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(to_enable, vec!["off-b"]);

        let to_disable: Vec<String> = selectable_routes(&config, Selectable::Enabled)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(to_disable, vec!["on-a", "on-c"]);

        assert_eq!(selectable_routes(&config, Selectable::Any).len(), 3);
    }

    #[test]
    fn a_disabled_route_says_so_in_the_picker() {
        let config = config_with(&[("off", false)]);
        let offered = selectable_routes(&config, Selectable::Any);
        assert!(offered[0].0.contains("[disabled]"), "{}", offered[0].0);
    }

    #[tokio::test]
    async fn an_explicit_name_is_never_second_guessed() {
        let config = config_with(&[("a", true), ("b", true)]);
        let picked = resolve_route_id(&config, Some("b".to_owned()), Selectable::Any)
            .await
            .unwrap();
        assert_eq!(picked, "b");
    }

    #[tokio::test]
    async fn having_no_routes_is_reported_as_such() {
        // Not as "every route is already disabled", which would be nonsense.
        let config = config_with(&[]);
        let err = resolve_route_id(&config, None, Selectable::Enabled)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no routes yet"), "{err}");
    }

    #[tokio::test]
    async fn nothing_left_to_toggle_says_why() {
        let config = config_with(&[("a", true)]);
        let err = resolve_route_id(&config, None, Selectable::Disabled)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("already enabled"), "{err}");
    }

    #[test]
    fn an_unknown_name_lists_the_names_that_do_exist() {
        // The user chose these names themselves, possibly weeks ago; a bare
        // "not found" leaves them guessing.
        let config = config_with(&[("news-mirror", true), ("market-alerts", true)]);
        let message = unknown_route(&config, "typo");

        assert!(message.contains("news-mirror"));
        assert!(message.contains("market-alerts"));
    }

    #[test]
    fn the_edit_menu_summarises_what_each_part_currently_is() {
        // The menu has to show the current value, or you cannot tell what you
        // are about to change.
        assert_eq!(describe_filter(&Filter::default()), "none");

        let filter = Filter {
            include: vec!["urgent".to_owned(), "快訊".to_owned()],
            exclude: vec!["ad".to_owned()],
            require_media: true,
            ..Filter::default()
        };
        let described = describe_filter(&filter);

        assert!(described.contains("2 required"), "{described}");
        assert!(described.contains("1 blocked"), "{described}");
        assert!(described.contains("media only"), "{described}");
    }

    #[test]
    fn keywords_are_split_and_trimmed() {
        assert_eq!(
            split_keywords(" urgent , breaking ,, "),
            vec!["urgent".to_owned(), "breaking".to_owned()]
        );
        assert_eq!(split_keywords("   "), Vec::<String>::new());
    }
}
