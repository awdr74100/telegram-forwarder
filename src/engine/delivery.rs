//! Getting a captured message into a target chat.
//!
//! Delivery is a ladder of three strategies, each weaker and more expensive than
//! the last, and each able to survive a failure the one above it cannot:
//!
//! 1. **Forward** — one RPC, keeps "Forwarded from" attribution, and does not
//!    re-upload anything. Fails if the source message is gone or the source chat
//!    restricts saving content.
//! 2. **Copy** — re-send as our own message, reusing the original media by
//!    reference. Immune to the source being deleted, still no re-upload. Fails
//!    if the file reference has gone stale.
//! 3. **Rehost** — re-send using the bytes captured by the snapshot. Works even
//!    when Telegram no longer acknowledges the original file at all.
//!
//! Which rungs are available is set by [`DeliveryMode`]; failures move down the
//! ladder, and only a failure at the bottom is a real failure.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use grammers_client::media::{Attribute, InputMedia, Uploaded};
use grammers_client::message::InputMessage;
use grammers_client::{Client, InvocationError};
use grammers_session::types::PeerRef;
use tokio::sync::Mutex;

use crate::config::{DeliveryMode, DispatchPolicy, MediaKind};

use super::failure::{self, Degrade, Disposition};
use super::router::{EchoGuard, Target};
use super::snapshot::{MediaCache, Snapshot};
use super::stats::{Stats, Strategy};

/// One or more messages delivered as a unit.
///
/// Telegram albums must be sent together or they arrive as unrelated messages,
/// so the album is the delivery unit rather than the individual message.
#[derive(Debug)]
pub struct Payload {
    /// The captured messages, in the order they were posted.
    pub snapshots: Vec<Arc<Snapshot>>,
    /// Reference to the chat they came from, needed for a native forward.
    pub source: PeerRef,
}

impl Payload {
    /// Whether this payload is a grouped album rather than a lone message.
    pub fn is_album(&self) -> bool {
        self.snapshots.len() > 1
    }

    /// Message IDs in the source chat.
    fn message_ids(&self) -> Vec<i32> {
        self.snapshots.iter().map(|snap| snap.message_id).collect()
    }

    /// The snapshot that carries the caption and drives logging.
    ///
    /// A payload always holds at least one snapshot, and the index says so
    /// rather than papering over it with a fallback that could never run. Three
    /// callers build one and every one of them is bounded: `dispatch` starts
    /// from a non-empty slice, `deliver_separately` builds single-member
    /// payloads, and the only reassignment inside `walk_ladder` comes from
    /// `Outcome::Partial`, which is returned solely when something was refused.
    fn primary(&self) -> &Arc<Snapshot> {
        &self.snapshots[0]
    }
}

/// What one send actually achieved.
///
/// Telegram answers a multi-message send with one slot per message and puts
/// `None` in the ones it refused, so "it returned Ok" and "everything arrived"
/// are different questions. Keeping the refused snapshots means the next rung
/// down can finish the job without re-sending what already landed.
#[derive(Debug, Default)]
struct Sent {
    /// IDs created in the target chat.
    delivered: Vec<i32>,
    /// Snapshots Telegram would not accept.
    refused: Vec<Arc<Snapshot>>,
}

impl Sent {
    /// Everything asked for arrived.
    fn all(delivered: Vec<i32>) -> Self {
        Self {
            delivered,
            refused: Vec::new(),
        }
    }

    /// Pair a per-message response with the snapshots it answers, splitting the
    /// accepted messages from the refused ones.
    ///
    /// A response shorter than the request is treated as a refusal of the
    /// remainder rather than as silence about it.
    fn split<'a>(
        results: &[Option<grammers_client::message::Message>],
        snapshots: impl IntoIterator<Item = &'a Arc<Snapshot>>,
    ) -> Self {
        let mut sent = Self::default();
        for (index, snapshot) in snapshots.into_iter().enumerate() {
            match results.get(index).and_then(Option::as_ref) {
                Some(message) => sent.delivered.push(message.id()),
                None => sent.refused.push(Arc::clone(snapshot)),
            }
        }
        sent
    }
}

/// Enforces a minimum gap between deliveries into the same chat.
///
/// Telegram rate-limits per destination, so this is keyed by target: fanning out
/// to ten chats at once is fine and is exactly what we want, while hammering one
/// chat earns a flood wait that would stall everything queued behind it.
#[derive(Debug)]
pub struct Pacer {
    interval: Duration,
    last_sent: Mutex<HashMap<i64, Instant>>,
}

impl Pacer {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_sent: Mutex::new(HashMap::new()),
        }
    }

    /// Wait until it is this target's turn, then claim the slot.
    ///
    /// The map holds the instant the most recent caller will send at, which may
    /// be in the future because the slot is reserved while the lock is still
    /// held. The next slot is therefore an interval after *that*, not an
    /// interval after now — waiting only from now collapses every caller queued
    /// behind a reservation onto the same instant.
    pub async fn acquire(&self, target_id: i64) {
        if self.interval.is_zero() {
            return;
        }

        let sleep_for = {
            let mut last = self.last_sent.lock().await;
            let now = Instant::now();

            // `max(now)` is what lets an idle chat go immediately: a reservation
            // already in the past imposes no wait at all.
            let slot = last
                .get(&target_id)
                .map_or(now, |previous| (*previous + self.interval).max(now));

            last.insert(target_id, slot);
            slot.saturating_duration_since(now)
        };

        if !sleep_for.is_zero() {
            tokio::time::sleep(sleep_for).await;
        }
    }
}

/// Shared machinery for delivering payloads.
pub struct Dispatcher {
    client: Client,
    stats: Arc<Stats>,
    echo: Arc<EchoGuard>,
    pacer: Pacer,
    policy: DispatchPolicy,
}

/// Hand-written because `grammers`' `Client` is not `Debug`.
impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    pub fn new(
        client: Client,
        stats: Arc<Stats>,
        echo: Arc<EchoGuard>,
        policy: DispatchPolicy,
    ) -> Self {
        Self {
            client,
            stats,
            echo,
            pacer: Pacer::new(policy.per_target_interval),
            policy,
        }
    }

    /// Deliver `payload` into `target`, walking down the strategy ladder.
    ///
    /// The payload shrinks as it goes: when a rung places part of an album and
    /// refuses the rest, only the remainder continues downwards. Re-sending the
    /// whole group would duplicate the messages that already arrived, and those
    /// duplicates would not be recognised by the echo guard.
    pub async fn deliver(
        &self,
        route: &str,
        payload: Payload,
        target: &Target,
        target_ref: PeerRef,
        mode: DeliveryMode,
    ) -> Result<()> {
        let describe = payload.primary().describe();
        let latency_from = Arc::clone(payload.primary());
        let total = payload.snapshots.len();
        let source = payload.source;

        let mut walk = self.walk_ladder(&payload, target, target_ref, mode).await;

        // A group Telegram will not take as a group can still be delivered as
        // its parts. Albums have to be homogeneous — a post mixing a photo with,
        // say, an SVG is one no rung can ever send as a single unit, and the
        // client that produced it warns about exactly this. Sending the members
        // one at a time gives up the grouping and nothing else, which beats
        // giving up the post.
        walk = match walk {
            Walk::Incomplete {
                remaining,
                reason,
                retry_smaller: true,
            } if remaining.len() > 1 => {
                tracing::warn!(
                    chat = %target.label,
                    parts = remaining.len(),
                    why = %reason,
                    "the group was refused as a group; delivering its parts separately"
                );
                self.deliver_separately(remaining, source, target, target_ref, mode)
                    .await
            }
            settled => settled,
        };

        let (strategy, rescued) = match walk {
            Walk::Delivered { strategy, rescued } => (strategy, rescued),
            Walk::Incomplete {
                remaining, reason, ..
            } => {
                return Err(self.report_failure(route, target, total, remaining.len(), &reason));
            }
        };

        let took = latency_from.captured_at.elapsed();
        self.stats.delivered(route, strategy, rescued);

        // Every delivery is announced, not just the interesting ones. Without
        // this the default presentation says nothing at all while it is working,
        // which reads exactly like a tool that is silently broken.
        let took = format!("{}ms", took.as_millis());
        if rescued {
            tracing::info!(
                ok = true,
                route = %route,
                via = strategy.label(),
                %took,
                what = %describe,
                "rescued into {} — the source was already gone",
                target.label
            );
        } else {
            tracing::info!(
                ok = true,
                route = %route,
                via = strategy.label(),
                %took,
                what = %describe,
                "delivered to {}",
                target.label
            );
        }

        Ok(())
    }

    /// Send each snapshot as a message of its own.
    ///
    /// Used only once the group has already been refused as a group. Anything
    /// that arrives here owes its delivery to the fallback, so it counts as a
    /// rescue even if the individual sends take the top rung.
    async fn deliver_separately(
        &self,
        parts: Vec<Arc<Snapshot>>,
        source: PeerRef,
        target: &Target,
        target_ref: PeerRef,
        mode: DeliveryMode,
    ) -> Walk {
        let mut carried_by = None;
        let mut missing = Vec::new();
        let mut reason = "no part of the group could be delivered".to_owned();

        for snapshot in parts {
            let single = Payload {
                snapshots: vec![Arc::clone(&snapshot)],
                source,
            };

            match self.walk_ladder(&single, target, target_ref, mode).await {
                Walk::Delivered { strategy, .. } => carried_by = carried_by.or(Some(strategy)),
                Walk::Incomplete { reason: why, .. } => {
                    reason = why;
                    missing.push(snapshot);
                }
            }
        }

        match carried_by {
            Some(strategy) if missing.is_empty() => Walk::Delivered {
                strategy,
                rescued: true,
            },
            // Already tried one at a time; there is nothing smaller left.
            _ => Walk::Incomplete {
                remaining: missing,
                reason,
                retry_smaller: false,
            },
        }
    }

    /// Walk the strategy ladder for one payload.
    ///
    /// The payload shrinks as it descends: when a rung places part of a group
    /// and refuses the rest, only the remainder continues downwards.
    async fn walk_ladder(
        &self,
        payload: &Payload,
        target: &Target,
        target_ref: PeerRef,
        mode: DeliveryMode,
    ) -> Walk {
        let total = payload.snapshots.len();
        let mut payload = Payload {
            snapshots: payload.snapshots.clone(),
            source: payload.source,
        };

        let mut last_error: Option<String> = None;
        // A part that landed on an earlier rung already owes its survival to the
        // snapshot, so finishing the job still counts as a rescue.
        let mut recovered_parts = false;

        for (index, strategy) in Self::ladder(mode).iter().copied().enumerate() {
            match self.attempt(&payload, target, target_ref, strategy).await {
                Outcome::Delivered => {
                    return Walk::Delivered {
                        strategy,
                        rescued: index > 0 || recovered_parts,
                    };
                }

                Outcome::Partial(remaining) => {
                    tracing::warn!(
                        target_chat = %target.label,
                        via = strategy.label(),
                        placed = total - remaining.len(),
                        missing = remaining.len(),
                        "part of a group was refused; sending only the rest"
                    );
                    last_error = Some(format!("{} part(s) were refused", remaining.len()));
                    payload.snapshots = remaining;
                    recovered_parts = true;
                }

                Outcome::Degraded(reason) => {
                    tracing::debug!(
                        target_chat = %target.label,
                        from = strategy.label(),
                        why = reason.reason(),
                        "falling back to a weaker delivery strategy"
                    );
                    last_error = Some(reason.reason().to_owned());
                }

                Outcome::Failed {
                    reason,
                    retry_smaller,
                } => {
                    return Walk::Incomplete {
                        remaining: payload.snapshots,
                        reason,
                        retry_smaller,
                    };
                }
            }
        }

        Walk::Incomplete {
            remaining: payload.snapshots,
            reason: last_error.unwrap_or_else(|| "every delivery strategy failed".to_owned()),
            // A rung having accepted part of the group is direct evidence that
            // its members are acceptable on their own. Without that evidence,
            // every rung refused the whole thing outright and splitting is a
            // guess paid for in extra requests.
            retry_smaller: recovered_parts,
        }
    }

    /// Record a failed delivery and build the error the caller logs.
    ///
    /// When part of an album did arrive, saying so is the difference between the
    /// user re-checking one message and re-checking the whole post.
    fn report_failure(
        &self,
        route: &str,
        target: &Target,
        total: usize,
        missing: usize,
        reason: &str,
    ) -> color_eyre::eyre::Report {
        let detail = if missing < total {
            format!(
                "{}: {} of {total} part(s) delivered, {missing} could not be: {reason}",
                target.label,
                total - missing
            )
        } else {
            format!("{}: {reason}", target.label)
        };

        self.stats.failed(route);
        color_eyre::eyre::eyre!("delivery to {detail}")
    }

    /// The strategies available under a given mode, strongest first.
    fn ladder(mode: DeliveryMode) -> Vec<Strategy> {
        let mut ladder = Vec::with_capacity(3);
        if mode.may_forward() {
            ladder.push(Strategy::Forward);
        }
        if mode.may_copy() {
            ladder.push(Strategy::Copy);
            ladder.push(Strategy::Rehost);
        }
        ladder
    }

    /// Run one strategy, retrying it while the failures are retryable.
    async fn attempt(
        &self,
        payload: &Payload,
        target: &Target,
        target_ref: PeerRef,
        strategy: Strategy,
    ) -> Outcome {
        for attempt in 0..self.policy.max_attempts {
            self.pacer.acquire(target.id).await;

            let result = match strategy {
                Strategy::Forward => self.send_forward(payload, target_ref).await,
                Strategy::Copy => self.send_copy(payload, target_ref).await,
                Strategy::Rehost => match self.send_rehost(payload, target_ref).await {
                    Ok(ids) => Ok(ids),
                    // A missing snapshot is not a Telegram failure, so it cannot
                    // be classified; it simply ends the ladder. Splitting the
                    // payload cannot conjure bytes that were never captured.
                    Err(RehostError::NoBytes) => {
                        return Outcome::Failed {
                            reason: "no snapshot available to re-upload".to_owned(),
                            retry_smaller: false,
                        };
                    }
                    Err(RehostError::Api(err)) => Err(err),
                },
            };

            match result {
                Ok(sent) => {
                    // Record what we produced so the router does not treat our
                    // own delivery as new source material. This happens even for
                    // a partial send: those messages exist in the target chat
                    // whether or not the rest of the group made it.
                    for id in &sent.delivered {
                        self.echo.remember(target.id, *id);
                    }

                    return match (sent.delivered.is_empty(), sent.refused.is_empty()) {
                        (_, true) => Outcome::Delivered,
                        // Nothing at all was accepted. That is an ordinary
                        // refusal of the whole post, not a partial success, and
                        // calling it partial would count a later rung's success
                        // as a rescue and report a misleading "0 of 3 parts".
                        (true, false) => Outcome::Degraded(Degrade::SourceGone),
                        (false, false) => Outcome::Partial(sent.refused),
                    };
                }

                Err(error) => match failure::classify(&error) {
                    Disposition::Wait(delay) => {
                        if delay > self.policy.max_flood_wait {
                            // Splitting a rate limit into more requests produces
                            // more rate limit, not delivery.
                            return Outcome::Failed {
                                reason: format!(
                                    "Telegram asked for a {}s wait, beyond the configured limit",
                                    delay.as_secs()
                                ),
                                retry_smaller: false,
                            };
                        }
                        tracing::warn!(
                            target_chat = %target.label,
                            seconds = delay.as_secs(),
                            "waiting out a rate limit"
                        );
                        tokio::time::sleep(delay).await;
                    }

                    Disposition::Backoff => {
                        tokio::time::sleep(failure::backoff_delay(attempt)).await;
                    }

                    Disposition::Degrade(reason) => return Outcome::Degraded(reason),

                    Disposition::Fatal(fatal) => {
                        return Outcome::Failed {
                            reason: fatal.reason.to_owned(),
                            retry_smaller: fatal.retry_smaller,
                        };
                    }
                },
            }
        }

        // The retry budget is spent. Spending it again per part would multiply
        // the same transient failure.
        Outcome::Failed {
            reason: format!("gave up after {} attempts", self.policy.max_attempts),
            retry_smaller: false,
        }
    }

    /// Strategy 1: a native Telegram forward.
    async fn send_forward(
        &self,
        payload: &Payload,
        target_ref: PeerRef,
    ) -> Result<Sent, InvocationError> {
        let ids = payload.message_ids();
        let results = self
            .client
            .forward_messages(target_ref, &ids, payload.source)
            .await?;

        // A forward can partially succeed: Telegram returns `None` for the
        // messages it refused. Treating that as success would silently lose half
        // an album, and treating it as total failure would duplicate the half
        // that arrived.
        Ok(Sent::split(&results, &payload.snapshots))
    }

    /// Strategy 2: re-send as our own message, reusing the media by reference.
    async fn send_copy(
        &self,
        payload: &Payload,
        target_ref: PeerRef,
    ) -> Result<Sent, InvocationError> {
        if payload.is_album() {
            // Only media Telegram can be asked to re-send by reference may
            // travel in the group. The test is `to_raw_input_media`, not merely
            // "has media": a link preview reports media but converts to nothing,
            // and `send_album` unwraps that, taking the process down with it.
            // Anything left out is handed to the rung below rather than dropped.
            let (grouped, loose): (Vec<_>, Vec<_>) = payload.snapshots.iter().partition(|snap| {
                snap.media
                    .as_ref()
                    .is_some_and(|media| media.to_raw_input_media().is_some())
            });

            let album: Vec<InputMedia> = grouped
                .iter()
                .filter_map(|snap| {
                    Some(
                        InputMedia::new()
                            .caption(snap.text.clone())
                            .fmt_entities(snap.entities.clone())
                            .copy_media(snap.media.as_ref()?),
                    )
                })
                .collect();

            if album.is_empty() {
                // Nothing in the group can be copied. Reporting the whole group
                // as refused sends it down the ladder intact; falling through to
                // the single-message path below would deliver one member and
                // call the rest done.
                return Ok(Sent {
                    delivered: Vec::new(),
                    refused: payload.snapshots.clone(),
                });
            }

            let results = self.client.send_album(target_ref, album).await?;
            let mut sent = Sent::split(&results, grouped);
            sent.refused.extend(loose.into_iter().map(Arc::clone));
            return Ok(sent);
        }

        let snap = payload.primary();
        let mut input = InputMessage::new()
            .text(snap.text.clone())
            .fmt_entities(snap.entities.clone());

        if let Some(media) = &snap.media {
            input = input.copy_media(media);
        }

        let sent = self.client.send_message(target_ref, input).await?;
        Ok(Sent::all(vec![sent.id()]))
    }

    /// Strategy 3: re-upload from the snapshotted bytes.
    ///
    /// An album is re-uploaded as an album. Sending only its first member would
    /// report success while quietly dropping the rest of the post, which is the
    /// exact failure this whole tool exists to prevent.
    async fn send_rehost(
        &self,
        payload: &Payload,
        target_ref: PeerRef,
    ) -> Result<Sent, RehostError> {
        if payload.is_album() {
            return self.rehost_album(payload, target_ref).await;
        }

        let snap = payload.primary();
        let Some(uploaded) = self.reupload(snap).await? else {
            // Nothing to upload. A message whose "media" never had bytes — a
            // poll, a location, a link preview — is still worth reproducing
            // from its text; one whose bytes are simply missing is not, because
            // the caption alone would misrepresent the post.
            if snap.await_media().await != MediaCache::NotApplicable || snap.text.is_empty() {
                return Err(RehostError::NoBytes);
            }

            let input = InputMessage::new()
                .text(snap.text.clone())
                .fmt_entities(snap.entities.clone());
            let sent = self
                .client
                .send_message(target_ref, input)
                .await
                .map_err(RehostError::Api)?;
            return Ok(Sent::all(vec![sent.id()]));
        };

        let mut input = InputMessage::new()
            .text(snap.text.clone())
            .fmt_entities(snap.entities.clone());

        // The MIME type has to be set before the media, and the file name after
        // it; otherwise both are ignored and the upload arrives as an unnamed
        // `application/octet-stream` blob.
        if let Some(mime) = &snap.mime_type {
            input = input.mime_type(mime);
        }

        // Photos must be sent as photos to stay viewable inline; everything else
        // travels as a document.
        input = if snap.kind == MediaKind::Photo {
            input.photo(uploaded)
        } else {
            input.document(uploaded)
        };

        if let Some(name) = &snap.file_name {
            input = input.attribute(Attribute::FileName(name.clone()));
        }

        let sent = self
            .client
            .send_message(target_ref, input)
            .await
            .map_err(RehostError::Api)?;

        Ok(Sent::all(vec![sent.id()]))
    }

    /// Re-upload every member of an album whose bytes survived, as one group.
    async fn rehost_album(
        &self,
        payload: &Payload,
        target_ref: PeerRef,
    ) -> Result<Sent, RehostError> {
        let mut album = Vec::with_capacity(payload.snapshots.len());
        let mut uploaded_from = Vec::with_capacity(payload.snapshots.len());
        let mut refused = Vec::new();

        for snap in &payload.snapshots {
            match self.reupload(snap).await? {
                Some(uploaded) => {
                    album.push(build_album_item(snap, uploaded));
                    uploaded_from.push(Arc::clone(snap));
                }
                None => refused.push(Arc::clone(snap)),
            }
        }

        if album.is_empty() {
            return Err(RehostError::NoBytes);
        }

        let results = self
            .client
            .send_album(target_ref, album)
            .await
            .map_err(RehostError::Api)?;

        let mut sent = Sent::split(&results, &uploaded_from);
        sent.refused.extend(refused);
        Ok(sent)
    }

    /// Upload a snapshot's cached bytes, if it has any.
    async fn reupload(&self, snap: &Snapshot) -> Result<Option<Uploaded>, RehostError> {
        let MediaCache::Ready(path) = snap.await_media().await else {
            return Ok(None);
        };

        self.client
            .upload_file(&path)
            .await
            .map(Some)
            .map_err(|err| RehostError::Api(InvocationError::Io(err)))
    }
}

/// One re-uploaded album member, with its original name and type restored.
fn build_album_item(snap: &Snapshot, uploaded: Uploaded) -> InputMedia {
    let mut item = InputMedia::new()
        .caption(snap.text.clone())
        .fmt_entities(snap.entities.clone());

    if let Some(mime) = &snap.mime_type {
        item = item.mime_type(mime);
    }

    item = if snap.kind == MediaKind::Photo {
        item.photo(uploaded)
    } else {
        item.document(uploaded)
    };

    if let Some(name) = &snap.file_name {
        item = item.attribute(Attribute::FileName(name.clone()));
    }

    item
}

/// How far a walk down the ladder got.
#[derive(Debug)]
enum Walk {
    /// Everything arrived, carried by this rung.
    Delivered { strategy: Strategy, rescued: bool },
    /// These parts did not arrive, for this reason.
    Incomplete {
        remaining: Vec<Arc<Snapshot>>,
        reason: String,
        /// Whether sending them one at a time is worth trying.
        retry_smaller: bool,
    },
}

/// Result of running one strategy to completion.
#[derive(Debug)]
enum Outcome {
    Delivered,
    /// Some of the group arrived; these are the parts that did not, and they
    /// are all the next rung should attempt.
    Partial(Vec<Arc<Snapshot>>),
    /// This strategy cannot work; try the next rung down.
    Degraded(Degrade),
    /// Nothing further will help for this payload as it stands.
    Failed {
        reason: String,
        /// Whether the same content might still be accepted in smaller pieces.
        retry_smaller: bool,
    },
}

/// Rehosting can fail for a reason that is not a Telegram error.
#[derive(Debug)]
enum RehostError {
    /// The snapshot has no bytes to re-upload.
    NoBytes,
    Api(InvocationError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_offers_the_whole_ladder() {
        assert_eq!(
            Dispatcher::ladder(DeliveryMode::Auto),
            vec![Strategy::Forward, Strategy::Copy, Strategy::Rehost]
        );
    }

    #[test]
    fn forward_mode_never_falls_back_to_copying() {
        // Deliberate: a user choosing `forward` wants attribution or nothing.
        assert_eq!(
            Dispatcher::ladder(DeliveryMode::Forward),
            vec![Strategy::Forward]
        );
    }

    #[test]
    fn copy_mode_skips_the_native_forward() {
        assert_eq!(
            Dispatcher::ladder(DeliveryMode::Copy),
            vec![Strategy::Copy, Strategy::Rehost]
        );
    }

    #[test]
    fn a_response_shorter_than_the_request_refuses_the_remainder() {
        // Telegram answers a multi-message send with one slot per message, but a
        // truncated response must not be read as "the rest were fine": that is
        // how half an album goes missing while the delivery reports success.
        let snapshots: Vec<Arc<Snapshot>> = (1..=3)
            .map(|id| Arc::new(Snapshot::for_test(id, "")))
            .collect();
        let sent = Sent::split(&[], &snapshots);

        assert_eq!(sent.delivered, Vec::<i32>::new());
        assert_eq!(sent.refused.len(), 3);
    }

    /// Whether `deliver` would break this refusal up and send the parts alone.
    ///
    /// Mirrors the guard in `deliver`; the point of the test is that the guard
    /// is narrow, because the fallback costs one request per part.
    fn would_split(walk: &Walk) -> bool {
        matches!(
            walk,
            Walk::Incomplete {
                remaining,
                retry_smaller: true,
                ..
            } if remaining.len() > 1
        )
    }

    fn refusal(parts: usize, reason: &str, retry_smaller: bool) -> Walk {
        Walk::Incomplete {
            remaining: (0..parts)
                .map(|id| Arc::new(Snapshot::for_test(i32::try_from(id).unwrap(), "")))
                .collect(),
            reason: reason.to_owned(),
            retry_smaller,
        }
    }

    #[test]
    fn a_rejected_group_is_delivered_as_its_parts() {
        // Albums have to be homogeneous, so a post mixing kinds is one no rung
        // can send as a unit even though every member is sendable alone.
        assert!(would_split(&refusal(3, "the request was rejected", true)));
    }

    #[test]
    fn a_refusal_that_smaller_requests_cannot_fix_is_not_split() {
        // This is the expensive mistake: the fallback costs one request per
        // part, so spending it where the destination or the rate limit is the
        // problem turns one refusal into several, during exactly the situation
        // that least needs more traffic.
        for reason in [
            "Telegram asked for a 600s wait, beyond the configured limit",
            "no permission to post in this chat",
            "this account is banned from the chat",
            "gave up after 5 attempts",
        ] {
            assert!(
                !would_split(&refusal(3, reason, false)),
                "{reason} must not be retried one part at a time"
            );
        }
    }

    #[test]
    fn a_single_refused_message_is_never_split() {
        // There are no parts to break it into; retrying would just repeat the
        // send that already failed.
        assert!(!would_split(&refusal(1, "the request was rejected", true)));
    }

    #[test]
    fn nothing_refused_means_fully_delivered() {
        let sent = Sent::all(vec![10, 11]);
        assert!(sent.refused.is_empty());
        assert_eq!(sent.delivered, vec![10, 11]);
    }

    #[tokio::test]
    async fn the_pacer_spaces_out_deliveries_to_one_chat() {
        let pacer = Pacer::new(Duration::from_millis(120));
        let start = Instant::now();

        pacer.acquire(-1001).await;
        pacer.acquire(-1001).await;

        assert!(
            start.elapsed() >= Duration::from_millis(110),
            "the second delivery should have waited"
        );
    }

    #[tokio::test]
    async fn the_pacer_does_not_delay_different_chats() {
        let pacer = Pacer::new(Duration::from_millis(200));
        let start = Instant::now();

        // Fanning out across many targets is the whole point; it must be fast.
        pacer.acquire(-1001).await;
        pacer.acquire(-1002).await;
        pacer.acquire(-1003).await;

        assert!(
            start.elapsed() < Duration::from_millis(100),
            "different chats must not queue behind each other"
        );
    }

    #[tokio::test]
    async fn concurrent_deliveries_to_one_chat_are_each_spaced() {
        // The real caller spawns one task per target, so several deliveries into
        // the same chat arrive here at once. Reserving from `now` rather than
        // from the slot already reserved let every caller after the second pick
        // the same instant, which is exactly the burst the pacer exists to stop.
        let pacer = Arc::new(Pacer::new(Duration::from_millis(120)));
        let start = Instant::now();

        let mut set = tokio::task::JoinSet::new();
        for _ in 0..3 {
            let pacer = Arc::clone(&pacer);
            set.spawn(async move { pacer.acquire(-1001).await });
        }
        while set.join_next().await.is_some() {}

        // Three deliveries means two gaps. Generous lower bound: the timer only
        // has to prove the third waited behind the second.
        assert!(
            start.elapsed() >= Duration::from_millis(200),
            "three deliveries to one chat finished in {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn a_chat_that_has_been_idle_is_not_made_to_wait() {
        // The reservation from a previous delivery is long past, so it must not
        // be turned into a fresh interval of waiting.
        // The bound is deliberately far from both sides of the question: a
        // regression makes this wait the full 200ms, and passing needs only that
        // it took under 50, which is a long time for a lock and some arithmetic.
        // An upper bound is the one shape of timing assertion a loaded CI runner
        // can break, so it is given an order of magnitude rather than a margin.
        let pacer = Pacer::new(Duration::from_millis(200));
        pacer.acquire(-1001).await;
        tokio::time::sleep(Duration::from_millis(260)).await;

        let start = Instant::now();
        pacer.acquire(-1001).await;
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "an idle chat waited {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn a_zero_interval_disables_pacing() {
        let pacer = Pacer::new(Duration::ZERO);
        let start = Instant::now();
        for _ in 0..50 {
            pacer.acquire(-1001).await;
        }
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
