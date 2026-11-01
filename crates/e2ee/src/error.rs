use crate::ids::DeviceRef;

/// Everything this crate refuses or fails at. No variant carries key material or plaintext.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Input that does not parse, or parses to something this protocol does not allow.
    #[error("malformed {0}")]
    Malformed(&'static str),
    /// Input larger than the limit for what it is.
    #[error("{0} is too large")]
    TooLarge(&'static str),
    #[error("unsupported version")]
    UnsupportedVersion,
    /// A signature (cross-signature) that does not verify.
    #[error("signature does not verify")]
    BadSignature,
    /// An authenticated-encryption check failed: wrong key, tampered, truncated or reordered data.
    #[error("authentication failed")]
    Authentication,
    #[error("randomness is unavailable")]
    Random,

    // Devices and trust.
    /// A person whose device list is not the one this client pinned, and not chained to it. The
    /// user interface must say so, and `Trust::accept_new_identity` re-pins after the person
    /// agrees.
    #[error("the person's identity changed")]
    IdentityChanged,
    /// Devices in a commit, a Welcome or a proposal this client has no trusted record for yet.
    /// Fetch and verify the person's device list, then try again.
    #[error("{} device(s) are not trusted", .0.len())]
    UntrustedDevices(Vec<DeviceRef>),

    // Groups.
    #[error("no group for this conversation")]
    NoGroup,
    #[error("a group for this conversation already exists")]
    GroupExists,
    /// This device was removed from the group.
    #[error("this device is not in the group")]
    NotAMember,
    /// The group cannot be used any more: it must be rebuilt (see `Error::EpochBackwards`).
    #[error("the group is frozen: {0}")]
    Frozen(&'static str),
    /// The server (or a rollback of its database) showed an epoch older than this device has
    /// already reached. The group is frozen.
    #[error("the epoch went backwards (local {local}, shown {shown})")]
    EpochBackwards { local: u64, shown: u64 },
    /// A message or commit for an epoch this device has not reached: fetch the handshake first.
    #[error("the message is for epoch {message}, this device is at {local}")]
    FutureEpoch { message: u64, local: u64 },
    /// A commit that skips an epoch.
    #[error("a commit is missing: have epoch {local}, commit is for {message}")]
    MissingCommit { message: u64, local: u64 },
    /// A commit or Welcome this device has already applied. Harmless.
    #[error("already applied")]
    Duplicate,
    /// The server's external sender key is not the one pinned for this conversation.
    #[error("the server's external sender key changed")]
    ServerKeyChanged,
    /// A commit of ours is waiting for the server's answer.
    #[error("a commit is in flight")]
    CommitInFlight,
    /// A commit that does something this protocol never does (an Update, a group context
    /// change, a credential change, a proposal type that is not an Add or a Remove).
    #[error("a commit that this protocol does not allow: {0}")]
    ForbiddenCommit(&'static str),
    /// A Remove proposal is waiting: commit it before sending.
    #[error("a Remove proposal is pending; commit it before sending")]
    PendingRemove,
    /// Proposals are waiting; commit them before sending.
    #[error("proposals are pending; commit them before sending")]
    PendingProposals,
    /// This device is itself named in a pending Remove and cannot commit it.
    #[error("this device is being removed")]
    SelfRemoval,

    // Messages.
    /// The sender the MLS layer authenticated is not the one the server claims, or an edit is
    /// not from the target's author.
    #[error("sender mismatch")]
    SenderMismatch,
    /// The message's authenticated data names another kind or target than the server shows.
    #[error("the message's kind or target does not match what the sender signed")]
    AadMismatch,
    /// The message cannot be opened with this device's state (a past epoch it no longer holds,
    /// a replay, a message from a group it never joined, a ciphertext that was altered).
    #[error("cannot be decrypted on this device")]
    Undecryptable,

    // Storage and I/O.
    #[error("i/o: {0}")]
    Io(String),
    #[error("storage: {0}")]
    Storage(String),
    /// An MLS library failure that none of the cases above covers. Its text is the library's.
    #[error("mls: {0}")]
    Mls(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn mls(err: impl std::fmt::Display) -> Error {
        Error::Mls(err.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Error {
        Error::Storage(err.to_string())
    }
}
