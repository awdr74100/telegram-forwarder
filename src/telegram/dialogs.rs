//! Enumerating the chats the logged-in account can see.
//!
//! This exists so that a user never has to type a chat ID by hand. Everything
//! selectable in the CLI comes from here.

use color_eyre::eyre::Result;
use grammers_client::Client;
use grammers_client::peer::Peer;
use grammers_session::Session;
use grammers_session::types::{PeerId, PeerRef};

use crate::session::FileSession;

/// What kind of chat an entry refers to, used for grouping and iconography.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChatKind {
    /// A broadcast channel.
    Channel,
    /// A supergroup or legacy group chat.
    Group,
    /// A one-to-one conversation with a bot.
    Bot,
    /// A one-to-one conversation with a person.
    User,
    /// The account's own "Saved Messages" chat.
    SavedMessages,
}

/// One selectable chat.
#[derive(Debug, Clone)]
pub struct DialogEntry {
    /// Bot API dialog ID, matching what is stored in the config file.
    pub id: i64,
    /// Display name.
    pub title: String,
    /// Public username without the `@`, if any.
    pub username: Option<String>,
    /// What sort of chat this is.
    pub kind: ChatKind,
    /// Whether this account may post here.
    ///
    /// Derived from flags already present in the dialog list, so it costs no
    /// extra API calls. It is exact for the case that actually bites people —
    /// a broadcast channel you can read but not publish to — and optimistic
    /// everywhere else, where the first delivery is the real test.
    pub likely_writable: bool,
}

/// Fetch every dialog the account has.
///
/// Besides producing the picker's contents, this warms the session's peer cache,
/// which `grammers` needs in order to resolve update gaps later. Running it at
/// least once after login is recommended by the library itself.
pub async fn fetch_all(client: &Client) -> Result<Vec<DialogEntry>> {
    let mut entries = Vec::new();
    let mut iter = client.iter_dialogs();

    while let Some(dialog) = iter.next().await? {
        let Some(entry) = entry_from_peer(dialog.peer()) else {
            continue;
        };
        entries.push(entry);
    }

    Ok(entries)
}

/// Convert a peer into a selectable entry, skipping anything unusable.
fn entry_from_peer(peer: &Peer) -> Option<DialogEntry> {
    let id = peer.id().bot_api_dialog_id()?;
    let username = peer.username().map(str::to_owned);

    let (kind, title, likely_writable) = match peer {
        Peer::Channel(channel) => {
            let raw = &channel.raw;
            // In a broadcast channel only the creator and admins holding the
            // post_messages right may publish. Everyone else can read only, and
            // finding that out at delivery time is a bad experience.
            let can_post = raw.creator
                || channel
                    .admin_rights()
                    .is_some_and(|rights| rights.post_messages);
            (
                ChatKind::Channel,
                channel.title().to_owned(),
                can_post && !raw.left,
            )
        }

        Peer::Group(group) => (
            ChatKind::Group,
            group.title().unwrap_or("(untitled group)").to_owned(),
            true,
        ),

        Peer::User(user) => {
            let kind = if user.is_self() {
                ChatKind::SavedMessages
            } else if user.is_bot() {
                ChatKind::Bot
            } else {
                ChatKind::User
            };
            let title = if user.is_self() {
                "Saved Messages".to_owned()
            } else {
                user.full_name()
            };
            (kind, title, true)
        }

        // A community only links other chats together; nothing is posted to
        // it, so it can be neither a source nor a target.
        Peer::Community(_) => return None,
    };

    Some(DialogEntry {
        id,
        title,
        username,
        kind,
        likely_writable,
    })
}

/// Resolve a configured chat ID into the reference the API needs.
///
/// Config files store plain IDs, but every Telegram call needs an ID *and* the
/// access hash bound to this account. That pairing lives in the session cache,
/// which [`fetch_all`] populates, so this is the bridge between the two.
///
/// Returns `None` when the chat is not in the session cache, which usually means
/// the account is no longer a member of it.
pub async fn resolve(session: &FileSession, chat_id: i64) -> Result<Option<PeerRef>> {
    let Some(peer_id) = PeerId::from_bot_api_dialog_id(chat_id) else {
        return Ok(None);
    };

    Ok(session.peer_ref(peer_id).await?)
}
