//! A single-file [`Session`] implementation.
//!
//! `grammers-session` ships two storages: an in-memory one that forgets the
//! authorization key on exit, and a `SQLite` one backed by `libsql`. Neither fits
//! here — losing the key means re-logging in, which is one of the most
//! flood-limited operations Telegram has, and pulling an embedded database in to
//! store what amounts to a few kilobytes of state is a poor trade.
//!
//! `Session` is a trait with nine methods, so this module implements it directly
//! over a JSON document. The result is a dependency-free, human-inspectable file
//! and a binary that needs no native toolchain to build.
//!
//! # Durability
//!
//! Peer caching fires constantly, so writing on every mutation would be wasteful.
//! Instead mutations set a dirty flag and [`FileSession::flush`] persists them.
//! The one exception is an authorization key arriving in [`Session::set_dc_option`]:
//! that is the expensive-to-recreate part, so it is written through immediately.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use grammers_session::types::{DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::{BoxFuture, Session, SessionData};
use serde::{Deserialize, Serialize};

/// Bumped when the on-disk shape changes incompatibly.
const FORMAT_VERSION: u32 = 1;

/// Errors produced while reading or writing the session file.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("session file {path} is not valid JSON: {source}")]
    Malformed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error(
        "session file {path} was written by format version {found}, but this build understands \
         version {expected}; delete it and log in again"
    )]
    UnsupportedVersion {
        path: PathBuf,
        found: u32,
        expected: u32,
    },

    #[error("the session lock was poisoned by a panic in another thread")]
    Poisoned,
}

/// The JSON document written to disk.
///
/// This mirrors [`SessionData`], which is not itself serializable, and adds a
/// version tag so a future format change can be detected instead of misparsed.
#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    home_dc: i32,
    dc_options: Vec<DcOption>,
    peers: Vec<PeerInfo>,
    updates_state: UpdatesState,
}

impl Document {
    fn from_data(data: &SessionData) -> Self {
        Self {
            version: FORMAT_VERSION,
            home_dc: data.home_dc,
            dc_options: data.dc_options.values().cloned().collect(),
            peers: data.peer_infos.values().cloned().collect(),
            updates_state: data.updates_state.clone(),
        }
    }

    fn into_data(self) -> SessionData {
        // Start from the default so statically-known datacenters are present even
        // if the file predates one being added.
        let mut data = SessionData {
            home_dc: self.home_dc,
            ..SessionData::default()
        };
        for option in self.dc_options {
            data.dc_options.insert(option.id, option);
        }
        data.peer_infos = self
            .peers
            .into_iter()
            .map(|peer| (peer.id(), peer))
            .collect();
        data.updates_state = self.updates_state;
        data
    }
}

/// A [`Session`] persisted as one JSON file.
pub struct FileSession {
    path: PathBuf,
    data: RwLock<SessionData>,
    dirty: AtomicBool,
}

/// Hand-written because [`SessionData`] is not `Debug`, and because the contents
/// are credentials that should never be printed by accident.
impl std::fmt::Debug for FileSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let peers = self.data.read().map(|data| data.peer_infos.len()).ok();
        f.debug_struct("FileSession")
            .field("path", &self.path)
            .field("cached_peers", &peers)
            .field("dirty", &self.dirty.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl FileSession {
    /// Load the session at `path`, or start a fresh one if the file is absent.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref().to_path_buf();

        let data = match fs_err::read(&path) {
            Ok(bytes) => {
                let document: Document =
                    serde_json::from_slice(&bytes).map_err(|source| SessionError::Malformed {
                        path: path.clone(),
                        source,
                    })?;

                if document.version != FORMAT_VERSION {
                    return Err(SessionError::UnsupportedVersion {
                        path,
                        found: document.version,
                        expected: FORMAT_VERSION,
                    });
                }

                document.into_data()
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => SessionData::default(),
            Err(source) => {
                return Err(SessionError::Io { path, source });
            }
        };

        Ok(Self {
            path,
            data: RwLock::new(data),
            dirty: AtomicBool::new(false),
        })
    }

    /// Persist the session if anything changed since the last write.
    ///
    /// Returns whether a write actually happened, which lets callers avoid
    /// logging a no-op.
    pub fn flush(&self) -> Result<bool, SessionError> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(false);
        }

        // Re-arm the flag if the write fails, so the data is not silently lost.
        match self.write_through() {
            Ok(()) => Ok(true),
            Err(err) => {
                self.dirty.store(true, Ordering::Release);
                Err(err)
            }
        }
    }

    /// Serialize and atomically replace the session file.
    fn write_through(&self) -> Result<(), SessionError> {
        let document = {
            let data = self.data.read().map_err(|_| SessionError::Poisoned)?;
            Document::from_data(&data)
        };

        let json =
            serde_json::to_vec_pretty(&document).map_err(|source| SessionError::Malformed {
                path: self.path.clone(),
                source,
            })?;

        if let Some(parent) = self.path.parent() {
            fs_err::create_dir_all(parent).map_err(|source| SessionError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        // Write to a sibling and rename, so an interrupted write cannot destroy a
        // working session.
        let tmp = self.path.with_extension("json.tmp");
        crate::private_file::write(&tmp, &json).map_err(|source| SessionError::Io {
            path: tmp.clone(),
            source,
        })?;

        fs_err::rename(&tmp, &self.path).map_err(|source| SessionError::Io {
            path: self.path.clone(),
            source,
        })?;

        Ok(())
    }

    /// Run `f` against the session data and mark the session dirty.
    fn mutate<T>(&self, f: impl FnOnce(&mut SessionData) -> T) -> Result<T, SessionError> {
        let mut data = self.data.write().map_err(|_| SessionError::Poisoned)?;
        let out = f(&mut data);
        drop(data);
        self.dirty.store(true, Ordering::Release);
        Ok(out)
    }
}

impl Session for FileSession {
    type Error = SessionError;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        let data = self.data.read().map_err(|_| SessionError::Poisoned)?;
        Ok(data.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move { self.mutate(|data| data.home_dc = dc_id) })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        let data = self.data.read().map_err(|_| SessionError::Poisoned)?;
        Ok(data.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            // An authorization key is expensive to recreate — logging in again is
            // heavily flood-limited — so this one mutation is written through
            // instead of waiting for the next flush.
            let carries_auth_key = dc_option.auth_key.is_some();
            self.mutate(|data| {
                data.dc_options.insert(dc_option.id, dc_option);
            })?;

            if carries_auth_key {
                self.dirty.store(false, Ordering::Release);
                if let Err(err) = self.write_through() {
                    self.dirty.store(true, Ordering::Release);
                    return Err(err);
                }
            }

            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move {
            let data = self.data.read().map_err(|_| SessionError::Poisoned)?;
            Ok(data.peer_infos.get(&peer).cloned())
        })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            self.mutate(|data| {
                data.peer_infos
                    .entry(peer.id())
                    .or_insert_with(|| peer.clone())
                    .extend_info(&peer);
            })
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async move {
            let data = self.data.read().map_err(|_| SessionError::Poisoned)?;
            Ok(data.updates_state.clone())
        })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move {
            self.mutate(|data| match update {
                UpdateState::All(state) => data.updates_state = state,
                UpdateState::Primary { pts, date, seq } => {
                    data.updates_state.pts = pts;
                    data.updates_state.date = date;
                    data.updates_state.seq = seq;
                }
                UpdateState::Secondary { qts } => data.updates_state.qts = qts,
                UpdateState::Channel { id, pts } => {
                    let channels = &mut data.updates_state.channels;
                    channels.retain(|channel| channel.id != id);
                    channels.push(grammers_session::types::ChannelState { id, pts });
                }
            })
        })
    }
}

/// Peers cached in the session, exposed for the dialog picker to reuse.
impl FileSession {
    /// Number of peers currently cached.
    pub fn cached_peer_count(&self) -> Result<usize, SessionError> {
        let data = self.data.read().map_err(|_| SessionError::Poisoned)?;
        Ok(data.peer_infos.len())
    }

    /// Whether an account is actually signed in on this session.
    ///
    /// The file existing is not the same thing: one is created as soon as any
    /// state is persisted, so reporting "signed in" from its presence alone
    /// tells the user the opposite of the truth after a failed login.
    pub fn has_authorization(&self) -> Result<bool, SessionError> {
        let data = self.data.read().map_err(|_| SessionError::Poisoned)?;
        Ok(data
            .dc_options
            .values()
            .any(|option| option.auth_key.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn absent_file_starts_a_fresh_session() {
        let dir = tempfile::tempdir().unwrap();
        let session = FileSession::load(dir.path().join("session.json")).unwrap();
        // A fresh session still knows the statically-configured datacenters.
        assert!(
            session
                .dc_option(session.home_dc_id().unwrap())
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn state_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");

        let session = FileSession::load(&path).unwrap();
        session.set_home_dc_id(4).await.unwrap();
        session
            .set_update_state(UpdateState::Primary {
                pts: 42,
                date: 7,
                seq: 3,
            })
            .await
            .unwrap();
        assert!(session.flush().unwrap());

        let reloaded = FileSession::load(&path).unwrap();
        assert_eq!(reloaded.home_dc_id().unwrap(), 4);
        assert_eq!(reloaded.updates_state().await.unwrap().pts, 42);
    }

    #[tokio::test]
    async fn flush_is_a_no_op_when_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let session = FileSession::load(dir.path().join("session.json")).unwrap();

        session.set_home_dc_id(2).await.unwrap();
        assert!(session.flush().unwrap(), "first flush writes");
        assert!(!session.flush().unwrap(), "second flush has nothing to do");
    }

    #[tokio::test]
    async fn peers_are_cached_and_merged() {
        let dir = tempfile::tempdir().unwrap();
        let session = FileSession::load(dir.path().join("session.json")).unwrap();

        let peer = PeerInfo::Channel {
            id: 1234,
            auth: None,
            kind: None,
        };
        session.cache_peer(peer.clone()).await.unwrap();

        assert_eq!(session.cached_peer_count().unwrap(), 1);
        assert!(session.peer(peer.id()).await.unwrap().is_some());
    }

    #[test]
    fn a_future_format_version_is_reported_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        fs_err::write(&path, br#"{"version":999,"home_dc":2,"dc_options":[],"peers":[],"updates_state":{"pts":0,"qts":0,"date":0,"seq":0,"channels":[]}}"#).unwrap();

        let err = FileSession::load(&path).unwrap_err();
        assert!(
            matches!(err, SessionError::UnsupportedVersion { found: 999, .. }),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_file_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let session = FileSession::load(&path).unwrap();
        session.set_home_dc_id(2).await.unwrap();
        session.flush().unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "session file must be owner-only");
    }

    #[tokio::test]
    async fn an_unauthorized_session_is_not_reported_as_signed_in() {
        // The file exists as soon as anything is persisted, so its presence says
        // nothing about whether a login ever succeeded.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let session = FileSession::load(&path).unwrap();
        session.set_home_dc_id(2).await.unwrap();
        session.flush().unwrap();

        assert!(path.exists(), "the file is written all the same");
        assert!(!session.has_authorization().unwrap());
    }
}
