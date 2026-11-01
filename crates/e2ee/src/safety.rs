//! Safety numbers: a short code two people compare out of band to know that each has the other's
//! real devices.
//!
//! The construction reuses DAVE's, which mediad's calls already show (`davey`): the pairwise
//! fingerprint (`daveprotocol.com/#verification-fingerprint`, scrypt N=2^14 r=8 p=2 over the two
//! parties' identities, sorted so both sides get the same bytes) and its displayable code
//! (`#displayable-codes`). DAVE has one key per person; a person here has a set of devices, so
//! each person's "key" is a SHA-256 over their user id and every device id and signing key,
//! sorted by device id:
//!
//! ```text
//! identity(u) = SHA-256("tiny-e2ee-v1 safety" || user_id(16) || n (u32 BE)
//!                       || for each device by device id: device_id(16) || signature_key(65))
//! fingerprint = DAVE pairwise_fingerprint(version 0,
//!                                         identity(a), first 8 bytes of a's id as u64,
//!                                         identity(b), first 8 bytes of b's id as u64)
//! code        = DAVE displayable_code(fingerprint, 60 digits, groups of 5)
//! ```
//!
//! So the number changes when either person gains or loses a device, and when any device's key
//! changes: a person who verified it sees it change. 60 digits carry about 199 bits. This is a
//! composition of established parts, and not a new primitive; it has not had an independent
//! review (see `docs/e2ee-core-notes.md`).

use crate::error::{Error, Result};
use crate::ids::{DeviceRef, UserId};
use sha2::{Digest, Sha256};

const LABEL: &[u8] = b"tiny-e2ee-v1 safety";
const DIGITS: u32 = 60;
const GROUP: u32 = 5;

/// A safety number.
#[derive(Clone, PartialEq, Eq)]
pub struct SafetyNumber {
    /// The 64 byte fingerprint: for a QR code, where comparing digits is not the point.
    pub fingerprint: [u8; 64],
    digits: String,
}

impl SafetyNumber {
    /// 60 digits, no spaces.
    pub fn digits(&self) -> &str {
        &self.digits
    }

    /// 12 groups of 5 digits, separated by spaces, for showing.
    pub fn grouped(&self) -> String {
        self.digits.as_bytes().chunks(GROUP as usize).map(|group| String::from_utf8_lossy(group).into_owned()).collect::<Vec<_>>().join(" ")
    }
}

impl std::fmt::Debug for SafetyNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SafetyNumber({})", self.grouped())
    }
}

fn identity(user: UserId, devices: &[DeviceRef]) -> Vec<u8> {
    let mut sorted: Vec<&DeviceRef> = devices.iter().filter(|device| device.user == user).collect();
    sorted.sort_by_key(|device| device.device);
    sorted.dedup_by_key(|device| device.device);
    let mut hasher = Sha256::new();
    hasher.update(LABEL);
    hasher.update(user.0);
    hasher.update((sorted.len() as u32).to_be_bytes());
    for device in sorted {
        hasher.update(device.device.0);
        hasher.update(&device.signature_key);
    }
    hasher.finalize().to_vec()
}

fn short(user: UserId) -> u64 {
    u64::from_be_bytes(user.0[..8].try_into().expect("a user id has 16 bytes"))
}

/// The safety number between `a` and `b`, from each one's devices (only the entries of
/// `a_devices` whose user is `a`, and of `b_devices` whose user is `b`, count). Symmetric: the
/// same for both people.
pub fn safety_number(a: UserId, a_devices: &[DeviceRef], b: UserId, b_devices: &[DeviceRef]) -> Result<SafetyNumber> {
    let ours = identity(a, a_devices);
    let theirs = identity(b, b_devices);
    let fingerprint = davey::generate_pairwise_fingerprint(0, &ours, short(a), &theirs, short(b)).map_err(|_| Error::Malformed("safety number input"))?;
    let digits = davey::generate_displayable_code(&fingerprint, DIGITS, GROUP).map_err(|_| Error::Malformed("safety number input"))?;
    let fingerprint: [u8; 64] = fingerprint.try_into().map_err(|_| Error::Malformed("safety number input"))?;
    Ok(SafetyNumber { fingerprint, digits })
}
