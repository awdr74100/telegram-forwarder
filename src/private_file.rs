//! Writing a file that only its owner can read.
//!
//! Both files this tool persists hold something that should not be readable by
//! other users of the machine: the session file *is* a login to the Telegram
//! account, and the configuration holds the API credentials. They are written
//! through here so the answer is the same for both, rather than the session
//! being careful and the configuration inheriting whatever the umask allows.
//!
//! The permissions are applied *as the file is created*, not afterwards. A
//! create-then-`chmod` leaves the contents on disk at the umask's discretion for
//! as long as the write takes, which is precisely the window worth closing.
//!
//! On platforms without Unix permissions this is an ordinary write: Windows
//! inherits the containing directory's ACL, and the directories this tool
//! creates are under the user's own profile.
//!
//! Every entry point unlinks the target first. `OpenOptions::mode` applies only
//! when the file is *created*, so writing over one that already exists would
//! silently keep whatever mode it had — and both callers write to a fixed
//! temporary name, which a killed process can leave behind.

use std::io;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// Create `path` empty, readable and writable by the owner alone.
///
/// For content this tool does not write itself: the media cache is filled by
/// `grammers`, which opens the path with `File::create` and therefore keeps the
/// mode of a file that is already there. Creating it first is what decides that
/// mode. Snapshotted media is the body of messages from chats the account can
/// see, which is no less private than the credentials next to it.
#[cfg(unix)]
pub fn create(path: &Path) -> io::Result<()> {
    open_private(path).map(drop)
}

/// Create `path` empty.
#[cfg(not(unix))]
pub fn create(path: &Path) -> io::Result<()> {
    fs_err::File::create(path).map(drop)
}

/// Write `bytes` to `path`, readable and writable by the owner alone.
#[cfg(unix)]
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_private(path)?;
    file.write_all(bytes)?;

    // A rename over this file is atomic, but only with respect to content that
    // actually reached the disk.
    file.sync_all()
}

/// Create `path` fresh, with owner-only permissions.
///
/// `fs_err` wraps free functions, not `OpenOptions`, and the mode has to be set
/// at creation rather than applied to an already-open file. The caller attaches
/// the path to any error, so nothing is lost by dropping to `std`.
#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<std::fs::File> {
    // Unlink first so the mode below is always the one that applies. Anything
    // already there is ours to replace: both callers pass a temporary they own,
    // and the media cache is keyed by chat and message id.
    match fs_err::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    // `create` rather than `create_new`: the unlink above is what makes the mode
    // apply, and refusing to open an existing file would only add a way to fail.
    // Two snapshots of one message can race for the same cache path — an update
    // replayed after a gap is captured again, since only messages this tool
    // *produced* are deduplicated — and the loser of that race would otherwise
    // lose its bytes and with them the bottom rung of the delivery ladder.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Write `bytes` to `path`.
#[cfg(not(unix))]
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs_err::write(path, bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn a_private_file_is_never_briefly_readable() {
        // Creating the file and then tightening it would leave the contents on
        // disk at the umask's discretion for as long as the write takes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        write(&path, b"key").unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o} exposes the file to other users");
    }

    #[test]
    fn writing_over_an_existing_file_replaces_its_permissions() {
        // `OpenOptions::mode` applies only when the file is created, so without
        // unlinking first a leftover would keep whatever mode it had. Both
        // callers write to a fixed temporary name, so a killed process leaves
        // exactly such a leftover behind.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing");

        fs_err::write(&path, b"old").unwrap();
        fs_err::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, b"new").unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o} kept the old file's exposure");
        assert_eq!(fs_err::read(&path).unwrap(), b"new");
    }

    #[test]
    fn a_created_file_is_empty_and_private() {
        // What the media cache relies on: `grammers` opens the path with
        // `File::create`, which keeps the mode of a file already there.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("media.bin");

        create(&path).unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o} exposes snapshotted media");
        assert_eq!(fs_err::read(&path).unwrap(), b"");
    }

    #[test]
    fn creating_the_same_path_twice_succeeds() {
        // One message can be captured more than once — an update replayed after
        // a gap arrives as a new message, and only messages this tool produced
        // are deduplicated — so two download tasks can share a cache path.
        // Refusing the second would cost it the bytes the rehost rung needs.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("twice.bin");

        create(&path).unwrap();
        create(&path).unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o} exposes snapshotted media");
    }

    #[test]
    fn creating_over_a_world_readable_leftover_tightens_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.bin");

        fs_err::write(&path, b"stale").unwrap();
        fs_err::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        create(&path).unwrap();

        let mode = fs_err::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o} kept the leftover's exposure");
    }
}
