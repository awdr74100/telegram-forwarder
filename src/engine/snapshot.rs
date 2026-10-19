//! Capturing a message the instant it arrives.
//!
//! This is the mechanism that makes deletion survivable. The moment an update
//! lands we copy everything that is already in memory — text, formatting
//! entities, and the media descriptor — which costs nothing and cannot fail.
//! Separately, and in the background, the media bytes are downloaded so that
//! even an expired file reference can be recovered from.
//!
//! The ordering matters. Delivery starts immediately against the *reference*;
//! the byte download is insurance that is only consulted if that fails.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use grammers_client::Client;
use grammers_client::media::{Downloadable as _, Media};
use grammers_client::message::Message;
use grammers_tl_types as tl;
use tokio::sync::watch;

use crate::config::{MediaKind, SnapshotPolicy};

use super::filter;

/// State of the background media download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaCache {
    /// The message carries no downloadable bytes.
    ///
    /// This covers plain text, but also polls, locations, contacts, dice and
    /// link previews: `grammers` models those as media, yet none of them has a
    /// file behind it. Telling them apart from a *failed* download matters,
    /// because a message that never had bytes can still be reproduced from its
    /// text alone, while one whose bytes are missing cannot.
    NotApplicable,
    /// The download is still running.
    Pending,
    /// Bytes are on disk at this path.
    Ready(PathBuf),
    /// The media exceeded the configured size limit and was not fetched.
    TooLarge,
    /// The download failed; only the reference can be used.
    Failed,
}

impl MediaCache {
    /// Whether the download has finished, one way or another.
    pub fn is_settled(&self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// Everything needed to reproduce a message without consulting Telegram again.
#[derive(Debug)]
pub struct Snapshot {
    /// Chat the message came from, as a Bot API dialog ID.
    pub source_chat: i64,
    /// Message ID within that chat.
    pub message_id: i32,
    /// Message text, or the media caption.
    pub text: String,
    /// Formatting entities, preserved so bold/links/spoilers survive a copy.
    pub entities: Vec<tl::enums::MessageEntity>,
    /// Media descriptor, usable to re-send without re-uploading.
    pub media: Option<Media>,
    /// Original MIME type. Without it a re-uploaded video is guessed from the
    /// cache file's extension and arrives as an unplayable binary blob.
    pub mime_type: Option<String>,
    /// Album identifier, when the message is part of a grouped post.
    pub grouped_id: Option<i64>,
    /// Classified payload kind.
    pub kind: MediaKind,
    /// Whether the message was itself forwarded from elsewhere.
    pub is_forward: bool,
    /// When the snapshot was taken, used to measure end-to-end latency.
    pub captured_at: Instant,
    /// Live state of the background byte download.
    media_state: watch::Receiver<MediaCache>,
}

impl Snapshot {
    /// Wait until the background download finishes, then report the outcome.
    ///
    /// Only the rehost path calls this: everything faster has already been tried
    /// by the time the bytes matter.
    pub async fn await_media(&self) -> MediaCache {
        let mut state = self.media_state.clone();
        // `wait_for` returns immediately when the current value already matches.
        state
            .wait_for(MediaCache::is_settled)
            .await
            .map_or(MediaCache::Failed, |value| value.clone())
    }

    /// A short human description used in logs.
    pub fn describe(&self) -> String {
        let preview: String = self.text.chars().take(40).collect();
        if preview.is_empty() {
            format!("{} message", self.kind)
        } else if self.text.chars().count() > 40 {
            format!("{preview}…")
        } else {
            preview
        }
    }
}

#[cfg(test)]
impl Snapshot {
    /// A settled, media-free snapshot, for tests in the sibling engine modules.
    ///
    /// `media_state` is private, so those modules cannot build one by hand.
    pub(super) fn for_test(message_id: i32, text: &str) -> Self {
        let (_tx, rx) = watch::channel(MediaCache::NotApplicable);
        Self {
            source_chat: -1001,
            message_id,
            text: text.to_owned(),
            entities: Vec::new(),
            media: None,
            mime_type: None,
            grouped_id: None,
            kind: MediaKind::Text,
            is_forward: false,
            captured_at: Instant::now(),
            media_state: rx,
        }
    }
}

/// Takes snapshots and owns the media cache directory.
pub struct Snapshotter {
    client: Client,
    cache_dir: PathBuf,
    policy: SnapshotPolicy,
}

/// Hand-written because `grammers`' `Client` is not `Debug`.
impl std::fmt::Debug for Snapshotter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshotter")
            .field("cache_dir", &self.cache_dir)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Snapshotter {
    pub fn new(client: Client, cache_dir: PathBuf, policy: SnapshotPolicy) -> Self {
        Self {
            client,
            cache_dir,
            policy,
        }
    }

    /// Capture a message.
    ///
    /// This is synchronous and allocation-only: it never awaits, so nothing can
    /// delay it past the point where the source might be deleted. If the message
    /// has media and the policy allows it, a download task is spawned.
    pub fn capture(&self, source_chat: i64, message: &Message) -> Arc<Snapshot> {
        let media = message.media();
        let kind = filter::classify(media.as_ref());
        let mime_type = mime_type_of(media.as_ref());

        // Only media with somewhere to download *from* is worth queueing. A poll
        // or a link preview reports `Some(media)` but has no file location, and
        // asking for it anyway leaves an empty file in the cache before the
        // download reports failure.
        let downloadable = media
            .as_ref()
            .is_some_and(|media| media.to_raw_input_location().is_some());

        let initial = if downloadable && self.policy.enabled {
            MediaCache::Pending
        } else {
            MediaCache::NotApplicable
        };
        let (tx, rx) = watch::channel(initial.clone());

        let snapshot = Arc::new(Snapshot {
            source_chat,
            message_id: message.id(),
            text: message.text().to_owned(),
            entities: message.fmt_entities().cloned().unwrap_or_default(),
            media: media.clone(),
            mime_type,
            grouped_id: message.grouped_id(),
            kind,
            is_forward: message.forward_header().is_some(),
            captured_at: Instant::now(),
            media_state: rx,
        });

        if initial == MediaCache::Pending
            && let Some(media) = media
        {
            self.spawn_download(&snapshot, media, tx);
        }

        snapshot
    }

    /// Download the media bytes in the background.
    fn spawn_download(
        &self,
        snapshot: &Arc<Snapshot>,
        media: Media,
        tx: watch::Sender<MediaCache>,
    ) {
        let size = media.size().unwrap_or(0) as u64;
        if self.policy.max_bytes > 0 && size > self.policy.max_bytes {
            // Large files rarely finish downloading inside the window a fast
            // deletion leaves, and re-uploading them is slow enough to hurt the
            // other targets waiting behind them.
            let _ = tx.send(MediaCache::TooLarge);
            return;
        }

        let path = self.cache_dir.join(format!(
            "{}_{}.bin",
            snapshot.source_chat, snapshot.message_id
        ));
        let client = self.client.clone();

        tokio::spawn(async move {
            // Created here rather than in `capture`, which must not perform a
            // blocking syscall: the whole point of capture is that nothing
            // delays it past the moment the source could be deleted.
            //
            // Created at all because `grammers` downloads through
            // `File::create`, which leaves the mode to the umask. What lands in
            // this file is the body of a message from a chat the account can
            // see, and it is no more public than the session sitting beside it.
            if let Err(error) = crate::private_file::create(&path) {
                tracing::debug!(%error, "could not create the snapshot file");
                let _ = tx.send(MediaCache::Failed);
                return;
            }

            let outcome = match client.download_media(&media, &path).await {
                Ok(()) => MediaCache::Ready(path),
                Err(error) => {
                    tracing::debug!(%error, "snapshot download failed");
                    MediaCache::Failed
                }
            };
            let _ = tx.send(outcome);
        });
    }

    /// Delete cached media older than the configured TTL.
    ///
    /// Called periodically by the engine. Directory listing and unlinking are
    /// blocking syscalls, and this runs on the same task that reads the update
    /// stream, so the work is handed to a blocking thread rather than stalling
    /// the thing that is racing a publisher's delete.
    pub fn sweep(&self) {
        let dir = self.cache_dir.clone();
        let ttl = self.policy.ttl;
        tokio::task::spawn_blocking(move || sweep_expired(&dir, ttl));
    }
}

/// The original MIME type, when the media has a file behind it.
///
/// A re-upload otherwise has its type guessed from the cache file's `.bin`
/// extension, which turns a video into an unplayable download.
fn mime_type_of(media: Option<&Media>) -> Option<String> {
    let document = match media {
        Some(Media::Document(document)) => document,
        Some(Media::Sticker(sticker)) => &sticker.document,
        _ => return None,
    };

    document.mime_type().map(str::to_owned)
}

/// Remove cache entries older than `ttl`.
///
/// Failures are ignored: a cache that cannot be swept is untidy, not broken.
fn sweep_expired(dir: &std::path::Path, ttl: std::time::Duration) {
    let Ok(entries) = fs_err::read_dir(dir) else {
        return;
    };

    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };

        if age > ttl {
            let _ = fs_err::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot with no media, wired to `state`.
    fn snapshot_with(state: watch::Receiver<MediaCache>, text: &str, kind: MediaKind) -> Snapshot {
        Snapshot {
            source_chat: -1001,
            message_id: 1,
            text: text.to_owned(),
            entities: Vec::new(),
            media: None,
            mime_type: None,
            grouped_id: None,
            kind,
            is_forward: false,
            captured_at: Instant::now(),
            media_state: state,
        }
    }

    #[test]
    fn pending_is_the_only_unsettled_state() {
        assert!(!MediaCache::Pending.is_settled());
        assert!(MediaCache::NotApplicable.is_settled());
        assert!(MediaCache::TooLarge.is_settled());
        assert!(MediaCache::Failed.is_settled());
        assert!(MediaCache::Ready(PathBuf::from("/tmp/x")).is_settled());
    }

    #[tokio::test]
    async fn awaiting_media_returns_once_the_download_settles() {
        let (tx, rx) = watch::channel(MediaCache::Pending);
        let snapshot = snapshot_with(rx, "", MediaKind::Photo);

        let handle = tokio::spawn(async move { snapshot.await_media().await });
        tx.send(MediaCache::Ready(PathBuf::from("/tmp/a.bin")))
            .unwrap();

        assert_eq!(
            handle.await.unwrap(),
            MediaCache::Ready(PathBuf::from("/tmp/a.bin"))
        );
    }

    #[tokio::test]
    async fn awaiting_an_already_settled_download_does_not_block() {
        let (_tx, rx) = watch::channel(MediaCache::NotApplicable);
        let snapshot = snapshot_with(rx, "", MediaKind::Text);

        assert_eq!(snapshot.await_media().await, MediaCache::NotApplicable);
    }

    #[test]
    fn sweeping_keeps_fresh_entries_and_drops_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("fresh.bin");
        fs_err::write(&fresh, b"x").unwrap();

        // A zero lifetime makes everything already expired; the config layer
        // rejects that combination, which is what this proves is worth doing.
        sweep_expired(dir.path(), std::time::Duration::ZERO);
        assert!(!fresh.exists(), "an expired entry should be removed");

        fs_err::write(&fresh, b"x").unwrap();
        sweep_expired(dir.path(), std::time::Duration::from_hours(1));
        assert!(fresh.exists(), "a fresh entry should survive");
    }

    #[test]
    fn describe_truncates_long_text() {
        let (_tx, rx) = watch::channel(MediaCache::NotApplicable);
        let snapshot = snapshot_with(rx, &"x".repeat(100), MediaKind::Text);

        let described = snapshot.describe();
        assert!(described.ends_with('…'));
        assert_eq!(described.chars().count(), 41);
    }

    #[test]
    fn describe_falls_back_to_the_media_kind() {
        let (_tx, rx) = watch::channel(MediaCache::NotApplicable);
        let snapshot = snapshot_with(rx, "", MediaKind::Photo);

        assert_eq!(snapshot.describe(), "photo message");
    }
}
