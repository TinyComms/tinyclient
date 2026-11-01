//! Measured costs of the e2ee core. Run in release:
//!
//! ```text
//! cargo run --release -p e2ee --example bench
//! cargo run --release -p e2ee --example bench -- 10 100 1000   # group sizes
//! ```
//!
//! Prints one table. Nothing here is an estimate: every number is a wall-clock time of the call
//! named, on the machine it ran on. State is kept in memory (no store), so what a snapshot costs is
//! measured on its own.

use e2ee::attachment::{self, AttachmentKey, Expect};
use e2ee::envelope::{Envelope, Kind};
use e2ee::identity::{DeviceKey, DeviceRecord};
use e2ee::store::StateKey;
use e2ee::{Claimed, ConversationId, Engine};
use openmls::prelude::tls_codec::{DeserializeBytes as _, Serialize as _};
use openmls::prelude::{
    ExternalProposal, GroupEpoch, GroupId, KeyPackageIn, LeafNodeIndex, MlsMessageOut,
    ProtocolVersion, SenderExtensionIndex,
};
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider as _;
use std::io::Read;
use std::time::{Duration, Instant};

fn time<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let value = work();
    (value, start.elapsed())
}

fn ms(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

fn per_op(duration: Duration, ops: u32) -> String {
    let micros = duration.as_secs_f64() * 1e6 / f64::from(ops);
    if micros >= 1000.0 {
        format!("{:.2} ms", micros / 1000.0)
    } else {
        format!("{micros:.0} us")
    }
}

struct Ds {
    key: DeviceKey,
    conv: ConversationId,
}

impl Ds {
    fn proposals(&self, adds: &[Vec<u8>], removes: &[u32], epoch: u64) -> Vec<u8> {
        let crypto = OpenMlsRustCrypto::default();
        let group = GroupId::from_slice(&self.conv.0);
        let mut messages: Vec<MlsMessageOut> = Vec::new();
        for leaf in removes {
            messages.push(
                ExternalProposal::new_remove::<OpenMlsRustCrypto>(
                    LeafNodeIndex::new(*leaf),
                    group.clone(),
                    GroupEpoch::from(epoch),
                    &self.key,
                    SenderExtensionIndex::new(0),
                )
                .unwrap(),
            );
        }
        for package in adds {
            let parsed = KeyPackageIn::tls_deserialize_exact_bytes(package)
                .unwrap()
                .validate(crypto.crypto(), ProtocolVersion::Mls10)
                .unwrap();
            messages.push(
                ExternalProposal::new_add::<OpenMlsRustCrypto>(
                    parsed,
                    group.clone(),
                    GroupEpoch::from(epoch),
                    &self.key,
                    SenderExtensionIndex::new(0),
                )
                .unwrap(),
            );
        }
        messages.tls_serialize_detached().unwrap()
    }
}

fn record(engine: &Engine) -> (e2ee::UserId, DeviceRecord) {
    (
        engine.user_id(),
        DeviceRecord {
            device_id: engine.device_id(),
            signature_key: engine.signature_key(),
            cross_signature: None,
            created_at: 1,
        },
    )
}

fn group_costs(n: usize) {
    println!("--- a group of {n} devices (one person each) ---");
    let ds = Ds {
        key: DeviceKey::generate().unwrap(),
        conv: ConversationId::random().unwrap(),
    };
    let conv = ds.conv;
    let server_key = ds.key.public_key();

    let (mut engines, took) = time(|| {
        (0..n)
            .map(|_| Engine::detached(e2ee::UserId::random().unwrap()).unwrap())
            .collect::<Vec<_>>()
    });
    println!(
        "make {n} devices (identity keys):      {} ({} each)",
        ms(took),
        per_op(took, n as u32)
    );
    let (packages, took) = time(|| {
        engines
            .iter_mut()
            .skip(1)
            .map(|engine| engine.key_packages(1).unwrap().remove(0))
            .collect::<Vec<_>>()
    });
    println!(
        "one key package per joiner:        {} ({} each, {} bytes)",
        ms(took),
        per_op(took, (n - 1).max(1) as u32),
        packages.first().map_or(0, Vec::len)
    );

    // Everyone trusts everyone (a device list each, trusted on first use).
    let records: Vec<_> = engines.iter().map(record).collect();
    let (_, took) = time(|| {
        for engine in engines.iter_mut().take(2) {
            for (user, record) in &records {
                engine
                    .verify_devices(*user, std::slice::from_ref(record))
                    .unwrap();
            }
        }
    });
    println!(
        "verify {n} device lists, twice:     {} ({} each)",
        ms(took),
        per_op(took, (2 * n) as u32)
    );
    // The other joiners need not trust anyone for this benchmark: only engine 0 and engine 1 act.

    engines[0].create_group(conv, &server_key).unwrap();
    let proposals = ds.proposals(&packages, &[], 0);
    let (outcome, took) = time(|| engines[0].handle_proposals(conv, &proposals).unwrap());
    assert_eq!(
        outcome.accepted.len(),
        n - 1,
        "{:?}",
        outcome.rejected.first()
    );
    println!(
        "take {} Add proposals ({} KiB):   {} ({} each)",
        n - 1,
        proposals.len() / 1024,
        ms(took),
        per_op(took, (n - 1).max(1) as u32)
    );
    let (bundle, took) = time(|| engines[0].commit_pending(conv).unwrap().unwrap());
    println!(
        "commit adding {} devices:          {} (commit {} bytes, Welcome {} bytes)",
        n - 1,
        ms(took),
        bundle.commit.len(),
        bundle.welcomes.first().map_or(0, |w| w.data.len())
    );
    engines[0].confirm_commit(conv).unwrap();
    if n > 1 {
        let (joined, took) =
            time(|| engines[1].handle_welcome(conv, &bundle.welcomes[0].data, Some(&server_key)));
        // Engine 1 trusts everyone only if it verified every list (it did above).
        let joined = joined.unwrap();
        println!(
            "join from the Welcome:             {} ({} members)",
            ms(took),
            joined.members.len()
        );
    }

    // One Remove in the full group: the cost that does not grow with the batch.
    if n > 2 {
        let last = engines[n - 1].device_id();
        let leaf = engines[0].leaf_index(conv, last).unwrap().unwrap();
        let proposals = ds.proposals(&[], &[leaf], 1);
        engines[0].handle_proposals(conv, &proposals).unwrap();
        engines[1].handle_proposals(conv, &proposals).unwrap();
        let (bundle, took) = time(|| engines[0].commit_pending(conv).unwrap().unwrap());
        println!(
            "commit removing 1 of {n}:           {} (commit {} bytes)",
            ms(took),
            bundle.commit.len()
        );
        engines[0].confirm_commit(conv).unwrap();
        let (_, took) = time(|| {
            engines[1]
                .handle_commit(conv, bundle.epoch, &bundle.commit)
                .unwrap()
        });
        println!("apply that commit:                 {}", ms(took));
        // A second one, now that the first commit has refreshed a path through the tree.
        let last = engines[n - 2].device_id();
        let leaf = engines[0].leaf_index(conv, last).unwrap().unwrap();
        let proposals = ds.proposals(&[], &[leaf], 2);
        engines[0].handle_proposals(conv, &proposals).unwrap();
        engines[1].handle_proposals(conv, &proposals).unwrap();
        let (bundle, took) = time(|| engines[0].commit_pending(conv).unwrap().unwrap());
        println!(
            "commit removing another:           {} (commit {} bytes)",
            ms(took),
            bundle.commit.len()
        );
        engines[0].confirm_commit(conv).unwrap();
        let (_, took) = time(|| {
            engines[1]
                .handle_commit(conv, bundle.epoch, &bundle.commit)
                .unwrap()
        });
        println!("apply that commit:                 {}", ms(took));
    }

    // Messages: a member sends, another opens.
    if n > 1 {
        for kib in [1usize, 4, 15] {
            let text = "x".repeat(kib * 1024 - 120);
            let envelope = Envelope {
                text,
                sent_at: Some(1_730_000_000_000),
                ..Envelope::default()
            };
            let rounds = 200u32;
            let mut sealed = Vec::new();
            let (_, took_encrypt) = time(|| {
                for _ in 0..rounds {
                    sealed.push(
                        engines[0]
                            .encrypt(conv, Kind::Message, None, &envelope)
                            .unwrap(),
                    );
                }
            });
            let claimed = Claimed {
                author: None,
                device: None,
                kind: Kind::Message,
                target_id: None,
            };
            let (_, took_decrypt) = time(|| {
                for message in &sealed {
                    engines[1]
                        .decrypt(conv, &message.ciphertext, &claimed)
                        .unwrap();
                }
            });
            println!(
                "{kib:>2} KiB message: encrypt {} (ciphertext {} bytes), decrypt {}",
                per_op(took_encrypt, rounds),
                sealed[0].ciphertext.len(),
                per_op(took_decrypt, rounds)
            );
        }
        let key = StateKey::generate().unwrap();
        let (blob, took) = time(|| engines[0].snapshot(&key, "bench").unwrap());
        println!(
            "seal a state snapshot:             {} ({} KiB)",
            ms(took),
            blob.len() / 1024
        );
        let (_, took) = time(|| Engine::from_snapshot(&key, "bench", &blob).unwrap());
        println!("open it again:                     {}", ms(took));
    }
    println!();
}

struct Pattern {
    state: u64,
    left: u64,
}

impl Read for Pattern {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let take = buf.len().min(self.left as usize);
        for chunk in buf[..take].chunks_mut(8) {
            self.state = self
                .state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let bytes = self.state.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        self.left -= take as u64;
        Ok(take)
    }
}

fn attachment_throughput() {
    println!("--- attachments (AES-256-GCM STREAM, 64 KiB chunks) ---");
    let key = AttachmentKey::generate().unwrap();
    for mib in [16u64, 128] {
        let len = mib << 20;
        let mut sealed = Vec::with_capacity(len as usize + (1 << 20));
        let (info, took_seal) = time(|| {
            attachment::seal(
                &key,
                b"bench",
                Pattern {
                    state: 1,
                    left: len,
                },
                &mut sealed,
            )
            .unwrap()
        });
        let (_, took_open) = time(|| {
            attachment::open(
                &key,
                b"bench",
                sealed.as_slice(),
                std::io::sink(),
                Expect {
                    sha256: Some(&info.sha256),
                    plain_len: Some(len),
                },
            )
            .unwrap()
        });
        println!(
            "{mib:>3} MiB: seal {:.0} MiB/s, open {:.0} MiB/s (includes SHA-256 of the ciphertext)",
            mib as f64 / took_seal.as_secs_f64(),
            mib as f64 / took_open.as_secs_f64()
        );
    }
    println!();
}

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|arg| arg.parse().ok())
        .collect();
    let sizes = if sizes.is_empty() {
        vec![10, 100, 1000]
    } else {
        sizes
    };
    println!(
        "e2ee bench, {} build, {}",
        if cfg!(debug_assertions) {
            "DEBUG"
        } else {
            "release"
        },
        std::env::consts::OS
    );
    println!();
    for n in sizes {
        group_costs(n);
    }
    attachment_throughput();
}
