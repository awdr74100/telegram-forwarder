//! Deciding what to do about a failed delivery.
//!
//! This is where most of the tool's usefulness lives. Telegram reports a wide
//! range of failures through one error type, and the right response differs
//! completely between them: some mean "wait", some mean "this approach will
//! never work, try another one", and some mean "give up".
//!
//! Getting this wrong is expensive in both directions. Retrying a permanent
//! failure burns rate limit for nothing; giving up on a recoverable one silently
//! drops a message the user wanted.

use std::time::Duration;

use grammers_client::InvocationError;

/// What should happen after a failed attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Telegram told us exactly how long to wait. Sleep, then retry as-is.
    Wait(Duration),

    /// This delivery strategy cannot work, but a weaker one might.
    ///
    /// The canonical case is the source message being deleted: a native forward
    /// is now impossible, but re-sending from the local snapshot still works.
    Degrade(Degrade),

    /// A transient fault with no server-supplied delay. Back off and retry.
    Backoff,

    /// Nothing will make this work as it stands. Report it and move on.
    Fatal(Fatal),
}

/// A refusal no retry will talk Telegram out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fatal {
    /// A short explanation for the user.
    pub reason: &'static str,

    /// Whether the same content might be accepted in smaller pieces.
    ///
    /// This is the difference between a rejected *request* and a rejected
    /// *destination*. One grouping the chat will not take, or one media kind
    /// among several it forbids, can still get through a piece at a time. A chat
    /// this account cannot write to refuses every piece just as firmly, and
    /// splitting a rate limit into more requests only produces more rate limit —
    /// so acting on the distinction is what keeps a fallback from making a bad
    /// situation worse.
    pub retry_smaller: bool,
}

impl Fatal {
    /// The destination or the account is the problem; smaller will not help.
    const fn destination(reason: &'static str) -> Self {
        Self {
            reason,
            retry_smaller: false,
        }
    }

    /// This particular request was refused; a smaller one might not be.
    const fn request(reason: &'static str) -> Self {
        Self {
            reason,
            retry_smaller: true,
        }
    }
}

/// Why a strategy was abandoned, and what it says about the next attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Degrade {
    /// The source message is gone. Only the local snapshot can serve it now.
    SourceGone,

    /// The source chat forbids forwarding its content.
    ///
    /// Re-sending our own copy is still allowed as long as we could read it.
    ForwardsRestricted,

    /// The stored file reference went stale, so the media must be re-uploaded
    /// from snapshotted bytes rather than referenced by ID.
    StaleFileReference,
}

impl Degrade {
    /// A short explanation for logs.
    pub fn reason(self) -> &'static str {
        match self {
            Self::SourceGone => "source message was deleted",
            Self::ForwardsRestricted => "source forbids forwarding",
            Self::StaleFileReference => "file reference expired",
        }
    }
}

/// Classify a failed API call.
///
/// `Duration` values come from Telegram itself; the caller decides whether a
/// requested wait is too long to honour.
pub fn classify(error: &InvocationError) -> Disposition {
    let InvocationError::Rpc(rpc) = error else {
        return match error {
            // The connection was torn down, usually because we are shutting
            // down. There is nothing left to retry against, in any size.
            InvocationError::Dropped => Disposition::Fatal(Fatal::destination("connection closed")),
            // Transport and IO faults are exactly what backoff is for.
            _ => Disposition::Backoff,
        };
    };

    // Every "wait N seconds" error carries N in `value`.
    //
    // Note the names have no trailing underscore: `grammers` strips the numeric
    // suffix out of the wire name (`FLOOD_WAIT_42` becomes `FLOOD_WAIT` plus
    // `value = 42`), so matching on `FLOOD_WAIT_*` would never fire.
    if rpc.is("FLOOD_WAIT")
        || rpc.is("FLOOD_PREMIUM_WAIT")
        || rpc.is("SLOWMODE_WAIT")
        || rpc.is("TAKEOUT_INIT_DELAY")
    {
        let seconds = u64::from(rpc.value.unwrap_or(1));
        return Disposition::Wait(Duration::from_secs(seconds));
    }

    // The source message no longer exists, so there is nothing to forward. This
    // is the exact scenario of a publisher deleting a post seconds after making
    // it, and it is recoverable only from a snapshot.
    if rpc.is("MESSAGE_ID_INVALID")
        || rpc.is("MESSAGE_IDS_EMPTY")
        || rpc.is("MSG_ID_INVALID")
        || rpc.is("RANDOM_ID_DUPLICATE")
    {
        return Disposition::Degrade(Degrade::SourceGone);
    }

    // The source channel has "restrict saving content" enabled.
    if rpc.is("CHAT_FORWARDS_RESTRICTED") || rpc.is("MESSAGE_AUTHOR_REQUIRED") {
        return Disposition::Degrade(Degrade::ForwardsRestricted);
    }

    // Referencing media by ID stopped working; the bytes must be re-uploaded.
    if rpc.is("FILE_REFERENCE_*")
        || rpc.is("MEDIA_EMPTY")
        || rpc.is("PHOTO_INVALID")
        || rpc.is("MEDIA_INVALID")
    {
        return Disposition::Degrade(Degrade::StaleFileReference);
    }

    // Permission and identity problems: no amount of retrying helps.
    if rpc.is("CHAT_WRITE_FORBIDDEN") {
        return Disposition::Fatal(Fatal::destination("no permission to post in this chat"));
    }
    // `is` only understands a leading or trailing `*`, so the family of
    // `CHAT_SEND_<kind>_FORBIDDEN` errors is matched by its common prefix. These
    // are per media kind, so a post carrying several kinds can still get its
    // permitted ones through one at a time.
    if rpc.is("CHAT_SEND_*") {
        return Disposition::Fatal(Fatal::request("this chat forbids that kind of media"));
    }
    if rpc.is("CHAT_ADMIN_REQUIRED") {
        return Disposition::Fatal(Fatal::destination("posting here requires admin rights"));
    }
    if rpc.is("CHANNEL_PRIVATE") {
        return Disposition::Fatal(Fatal::destination(
            "this account is not a member of the chat",
        ));
    }
    if rpc.is("USER_BANNED_IN_CHANNEL") {
        return Disposition::Fatal(Fatal::destination("this account is banned from the chat"));
    }
    if rpc.is("PEER_ID_INVALID") {
        return Disposition::Fatal(Fatal::destination("the chat is unknown to this account"));
    }
    if rpc.is("AUTH_KEY_*") || rpc.is("SESSION_REVOKED") || rpc.is("USER_DEACTIVATED*") {
        return Disposition::Fatal(Fatal::destination(
            "the session is no longer valid; log in again",
        ));
    }
    // One overlong caption sinks the whole request, but each piece carries only
    // its own.
    if rpc.is("MEDIA_CAPTION_TOO_LONG") {
        return Disposition::Fatal(Fatal::request(
            "the caption is too long for this account tier",
        ));
    }

    // A server-side fault is worth retrying; anything else in the 400 range is
    // a request we should not repeat unchanged — though a different, smaller
    // one is not the same request.
    if rpc.code >= 500 {
        Disposition::Backoff
    } else {
        Disposition::Fatal(Fatal::request("the request was rejected"))
    }
}

/// Exponential backoff with jitter, for failures that carry no server delay.
///
/// Jitter matters when several targets fail together: without it they would all
/// wake at the same instant and hammer the API in lockstep.
pub fn backoff_delay(attempt: u32) -> Duration {
    use rand::RngExt as _;

    const BASE_MS: u64 = 400;
    const CEILING_MS: u64 = 30_000;

    // The clamp on the shift is only there to keep it inside `u64`; the ceiling
    // is what actually bounds the wait. Clamping the shift at 6 instead, as this
    // once did, capped the delay at 25.6s and left `CEILING_MS` describing a
    // limit nothing could ever reach.
    let exponential = BASE_MS.saturating_mul(1u64 << attempt.min(32));
    let capped = exponential.min(CEILING_MS);
    let jitter = rand::rng().random_range(0..=capped / 4);

    Duration::from_millis(capped + jitter)
}

#[cfg(test)]
mod tests {
    use grammers_client::sender::RpcError;

    use super::*;

    /// Build an error the way the network actually delivers it.
    ///
    /// Going through `RpcError::from` rather than constructing the struct by
    /// hand is deliberate: it is what strips `_42` off `FLOOD_WAIT_42` into the
    /// separate `value` field. A test that skipped this step would happily pass
    /// against matching rules that never fire in production.
    fn rpc(name: &str, code: i32) -> InvocationError {
        InvocationError::Rpc(RpcError::from(grammers_client::tl::types::RpcError {
            error_code: code,
            error_message: name.to_owned(),
        }))
    }

    #[test]
    fn the_wire_format_splits_the_delay_out_of_the_error_name() {
        // Guards the exact trap described above.
        let InvocationError::Rpc(parsed) = rpc("FLOOD_WAIT_42", 420) else {
            panic!("expected an RPC error");
        };
        assert_eq!(parsed.name, "FLOOD_WAIT");
        assert_eq!(parsed.value, Some(42));
        assert!(
            !parsed.is("FLOOD_WAIT_*"),
            "the underscore form must not match"
        );
        assert!(parsed.is("FLOOD_WAIT"));
    }

    #[test]
    fn flood_wait_is_honoured_with_the_server_supplied_delay() {
        assert_eq!(
            classify(&rpc("FLOOD_WAIT_42", 420)),
            Disposition::Wait(Duration::from_secs(42))
        );
    }

    #[test]
    fn slowmode_is_treated_as_a_wait_too() {
        assert_eq!(
            classify(&rpc("SLOWMODE_WAIT_10", 420)),
            Disposition::Wait(Duration::from_secs(10))
        );
    }

    #[test]
    fn the_chat_send_forbidden_family_is_matched_by_prefix() {
        for name in [
            "CHAT_SEND_MEDIA_FORBIDDEN",
            "CHAT_SEND_PHOTOS_FORBIDDEN",
            "CHAT_SEND_DOCS_FORBIDDEN",
        ] {
            assert!(
                matches!(classify(&rpc(name, 403)), Disposition::Fatal(_)),
                "{name} should be fatal"
            );
        }
    }

    #[test]
    fn a_deleted_source_degrades_to_the_snapshot() {
        // This is the headline scenario: the post was removed before we could
        // forward it.
        assert_eq!(
            classify(&rpc("MESSAGE_ID_INVALID", 400)),
            Disposition::Degrade(Degrade::SourceGone)
        );
    }

    #[test]
    fn a_forward_restricted_channel_degrades_to_copying() {
        assert_eq!(
            classify(&rpc("CHAT_FORWARDS_RESTRICTED", 400)),
            Disposition::Degrade(Degrade::ForwardsRestricted)
        );
    }

    #[test]
    fn a_stale_file_reference_forces_a_re_upload() {
        assert_eq!(
            classify(&rpc("FILE_REFERENCE_EXPIRED", 400)),
            Disposition::Degrade(Degrade::StaleFileReference)
        );
    }

    #[test]
    fn permission_errors_are_fatal_rather_than_retried() {
        for name in [
            "CHAT_WRITE_FORBIDDEN",
            "CHAT_ADMIN_REQUIRED",
            "CHANNEL_PRIVATE",
            "USER_BANNED_IN_CHANNEL",
        ] {
            assert!(
                matches!(classify(&rpc(name, 403)), Disposition::Fatal(_)),
                "{name} should be fatal"
            );
        }
    }

    #[test]
    fn server_errors_are_retried_but_client_errors_are_not() {
        assert_eq!(
            classify(&rpc("INTERNAL_SERVER_ERROR", 500)),
            Disposition::Backoff
        );
        assert!(matches!(
            classify(&rpc("SOMETHING_ODD", 400)),
            Disposition::Fatal(_)
        ));
    }

    #[test]
    fn a_dropped_connection_is_not_retried() {
        assert!(matches!(
            classify(&InvocationError::Dropped),
            Disposition::Fatal(_)
        ));
    }

    #[test]
    fn backoff_grows_and_then_stops_growing() {
        let first = backoff_delay(0);
        let later = backoff_delay(4);
        assert!(later > first);

        // The ceiling plus its maximum jitter.
        let capped = backoff_delay(20);
        assert!(capped <= Duration::from_millis(30_000 + 30_000 / 4));

        // And it is reached, rather than being a limit the growth stops short
        // of: without this, clamping the shift too low would silently cap the
        // wait somewhere below the figure the constant claims.
        assert!(capped >= Duration::from_secs(30));
    }

    #[test]
    fn an_absurd_attempt_number_does_not_overflow_the_shift() {
        // `max_attempts` is user-supplied, so nothing stops the counter from
        // reaching a value that would panic on a shift in a debug build. Calling
        // it at all is most of this test; the bound is the rest.
        assert!(backoff_delay(u32::MAX) <= Duration::from_millis(30_000 + 30_000 / 4));
    }
}
