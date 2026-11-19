//! The `Journal` and `Relay` capabilities the kernel delegates to its
//! host ([`Kernel::with_journal`], [`Kernel::with_relay`]): refused
//! without them, introspected, and with an in-memory host journal (a
//! window and a pump thread, after bus-contract's reference
//! implementation) entries delivered to the kernel's listeners in order,
//! off the publisher's thread, local listeners at their thread's
//! `run_pending`; a relay read from a cursor and taken in on another
//! kernel.

#[path = "support/memory.rs"]
mod memory;

use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use bus_kernel::bus_contract::{Bus, PointKey};
use bus_kernel::{
    Answer, Capability, Codec, Cursor, IngestError, Kernel, ListenOpts, Lost, Origin, Point,
    PointDecl, PointKind, PointSpec, Refused, Relayed, RelayedAnswer, RelayedAsk, RelayedEntry,
    RelayedRequest, declare_all, offers,
};
use memory::{Memory, full, kernel};

// --- The points of the tests ---

#[derive(Clone, Debug, PartialEq)]
struct Imported {
    path: String,
    count: u32,
}

/// Photos were imported. Relayable.
struct LibraryImported;
impl Point for LibraryImported {
    const ID: &'static str = "library.imported";
    const KIND: PointKind = PointKind::Event;
    type Input = Imported;
    type Value = ();
}

/// `path|count`; extra fields after them are ignored (a newer sender).
impl Relayed for LibraryImported {
    fn encode(input: &Imported) -> String {
        format!("{}|{}", input.path, input.count)
    }
    fn decode(text: &str) -> Result<Imported, String> {
        let mut parts = text.split('|');
        let path = parts.next().ok_or("no path")?.to_owned();
        let count = parts
            .next()
            .ok_or("no count")?
            .parse()
            .map_err(|e| format!("count: {e}"))?;
        Ok(Imported { path, count })
    }
}

/// A journaled event kept in its process.
struct StepComputed;
impl Point for StepComputed {
    const ID: &'static str = "engine.step";
    const KIND: PointKind = PointKind::Event;
    type Input = u32;
    type Value = ();
}

/// An event emitted directly, not journaled.
struct VolumeMounted;
impl Point for VolumeMounted {
    const ID: &'static str = "volume.mounted";
    const KIND: PointKind = PointKind::Event;
    type Input = String;
    type Value = ();
}

/// A request: never journaled.
struct DocClose;
impl Point for DocClose {
    const ID: &'static str = "doc.close";
    const KIND: PointKind = PointKind::Request;
    type Input = String;
    type Value = Vec<String>;
}

const POINTS: &[PointDecl] = &[
    PointDecl::of::<LibraryImported>("Photos were imported.")
        .relayable(Codec::of::<LibraryImported>()),
    PointDecl::of::<StepComputed>("A render step was computed.").journaled(),
    PointDecl::of::<VolumeMounted>("A volume was mounted."),
];

fn imported(path: &str, count: u32) -> Imported {
    Imported {
        path: path.into(),
        count,
    }
}

const WAIT: Duration = Duration::from_secs(10);

// --- Capabilities ---

#[test]
fn without_a_host_journal_journaled_and_relayable_points_are_refused() {
    let k = Kernel::new();
    assert!(!offers(&k, Capability::Journal));
    assert!(!offers(&k, Capability::Relay));
    assert!(Bus::journal(&k).is_none());
    assert!(Bus::relay(&k).is_none());
    assert_eq!(
        declare_all(&k, POINTS),
        [
            Refused::Unsupported {
                point: "library.imported",
                capability: Capability::Journal
            },
            Refused::Unsupported {
                point: "engine.step",
                capability: Capability::Journal
            },
        ]
    );
    // Declared directly, past `declare_all`: refused the same.
    assert_eq!(
        k.declare::<StepComputed>(PointSpec::new("Steps.").journaled()),
        Err(Refused::Unsupported {
            point: "engine.step",
            capability: Capability::Journal
        })
    );
    let ids: Vec<_> = k.points().iter().map(|p| p.id).collect();
    assert_eq!(ids, ["volume.mounted"]);
}

#[test]
fn a_relay_without_a_journal_is_not_offered() {
    let host = Arc::new(Memory::new(Weak::new(), 16));
    let k = Kernel::new().with_relay(host);
    assert!(!offers(&k, Capability::Relay));
    assert!(Bus::relay(&k).is_none());
}

#[test]
fn a_journal_without_a_relay_refuses_relayable_points() {
    let k = kernel(false, 16);
    assert!(offers(&*k, Capability::Journal));
    assert!(!offers(&*k, Capability::Relay));
    assert_eq!(
        declare_all(&*k, POINTS),
        [Refused::Unsupported {
            point: "library.imported",
            capability: Capability::Relay
        }]
    );
    assert_eq!(
        k.declare::<LibraryImported>(
            PointSpec::new("Imports.").relayable(Codec::of::<LibraryImported>())
        ),
        Err(Refused::Unsupported {
            point: "library.imported",
            capability: Capability::Relay
        })
    );
}

#[test]
fn only_an_event_point_is_journaled_and_a_codec_fits_its_point() {
    let k = full();
    assert!(offers(&*k, Capability::Relay));
    assert_eq!(
        k.declare::<DocClose>(PointSpec::new("Closes.").journaled()),
        Err(Refused::NotEvent { point: "doc.close" })
    );
    assert_eq!(
        k.declare::<VolumeMounted>(
            PointSpec::new("Volumes.").relayable(Codec::of::<LibraryImported>())
        ),
        Err(Refused::TypeMismatch {
            point: "volume.mounted"
        })
    );
    assert!(k.points().is_empty());
}

#[test]
fn introspection_and_the_declaration_show_journaled_and_relayable_points() {
    let k = full();
    assert!(declare_all(&*k, POINTS).is_empty());
    let shown: Vec<_> = k
        .points()
        .iter()
        .map(|p| (p.id, p.journaled, p.relayable))
        .collect();
    assert_eq!(
        shown,
        [
            ("engine.step", true, false),
            ("library.imported", true, true),
            ("volume.mounted", false, false),
        ]
    );
    let (key, spec) = k.declaration(LibraryImported::ID).unwrap();
    assert_eq!(key, PointKey::of::<LibraryImported>());
    assert_eq!(spec.relay, Some(Codec::of::<LibraryImported>()));
    assert!(k.declaration("nobody").is_none());
    assert!(format!("{k:?}").contains("journal: true"));
}

// --- Journal ---

#[test]
fn entries_are_delivered_once_in_order_off_the_publishers_thread() {
    let k = full();
    assert!(declare_all(&*k, POINTS).is_empty());
    let heard: Arc<Mutex<Vec<(u32, ThreadId)>>> = Arc::default();
    let h = heard.clone();
    k.listen::<StepComputed>("ui.steps", ListenOpts::new(), move |n, _| {
        h.lock().unwrap().push((*n, thread::current().id()));
        Ok(())
    })
    .unwrap();

    let journal = Bus::journal(&*k).unwrap();
    assert_eq!(journal.last_seq(), 0);
    let seqs: Vec<u64> = (0..5)
        .map(|n| journal.publish::<StepComputed>(n).unwrap())
        .collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    assert_eq!(journal.last_seq(), 5);
    assert!(journal.flush(WAIT));
    let heard = heard.lock().unwrap();
    let order: Vec<u32> = heard.iter().map(|h| h.0).collect();
    assert_eq!(order, [0, 1, 2, 3, 4]);
    assert!(heard.iter().all(|h| h.1 != thread::current().id()));
    // Read again, typed, and waited for.
    let read = journal.since(3, 10);
    assert_eq!(read.entries.len(), 2);
    assert_eq!(read.entries[0].read::<StepComputed>(), Some(&3));
    assert_eq!(journal.wait_for::<StepComputed>(4, WAIT), Some((5, 4)));
}

#[test]
fn a_local_listener_hears_entries_at_its_threads_run_pending_in_order() {
    let k = full();
    assert!(declare_all(&*k, POINTS).is_empty());
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    k.listen_local::<StepComputed>("ui.local", ListenOpts::new(), move |n, _| {
        h.lock().unwrap().push(*n);
        Ok(())
    })
    .unwrap();
    let journal = Bus::journal(&*k).unwrap();
    for n in 0..4 {
        journal.publish::<StepComputed>(n).unwrap();
    }
    assert!(journal.flush(WAIT));
    // Queued for this thread by the pump, not run on it.
    assert!(heard.lock().unwrap().is_empty());
    let runs = k.run_pending();
    assert_eq!(runs.len(), 4);
    assert!(runs.iter().all(|r| r.ran == ["ui.local"]));
    assert_eq!(*heard.lock().unwrap(), [0, 1, 2, 3]);
}

#[test]
fn a_slow_listener_delays_the_next_entries_not_the_publisher() {
    let k = full();
    assert!(declare_all(&*k, POINTS).is_empty());
    k.listen::<StepComputed>("slow", ListenOpts::new(), |_, _| {
        thread::sleep(Duration::from_millis(200));
        Ok(())
    })
    .unwrap();
    let journal = Bus::journal(&*k).unwrap();
    let start = Instant::now();
    for n in 0..3 {
        journal.publish::<StepComputed>(n).unwrap();
    }
    let published = start.elapsed();
    assert!(journal.flush(WAIT));
    assert!(start.elapsed() >= Duration::from_millis(600));
    assert!(
        published < Duration::from_millis(200),
        "the publisher waited on the listener: {published:?}"
    );
}

#[test]
fn the_kernel_refuses_a_publish_on_a_point_not_journaled() {
    let k = full();
    assert!(declare_all(&*k, POINTS).is_empty());
    let journal = Bus::journal(&*k).unwrap();
    assert_eq!(
        journal.publish::<VolumeMounted>("/mnt".into()),
        Err(Refused::NotJournaled {
            point: "volume.mounted"
        })
    );
    assert_eq!(
        journal.publish::<DocClose>("a".into()),
        Err(Refused::NotJournaled { point: "doc.close" })
    );
    // Nothing reached the host.
    assert_eq!(journal.last_seq(), 0);
}

// --- Relay ---

fn node_origin() -> Origin {
    Origin::process("node")
}

type Heard = Arc<Mutex<Vec<Imported>>>;

fn node_and_desktop() -> (Arc<Kernel>, Arc<Kernel>, Heard) {
    let node = full();
    let desktop = full();
    assert!(declare_all(&*node, POINTS).is_empty());
    assert!(declare_all(&*desktop, POINTS).is_empty());
    let heard: Heard = Arc::default();
    let h = heard.clone();
    desktop
        .listen::<LibraryImported>("desktop.grid", ListenOpts::new(), move |i, _| {
            h.lock().unwrap().push(i.clone());
            Ok(())
        })
        .unwrap();
    (node, desktop, heard)
}

#[test]
fn relayable_events_resume_on_one_kernel_and_are_ingested_on_another() {
    let (node, desktop, heard) = node_and_desktop();
    let nj = Bus::journal(&*node).unwrap();
    let a = nj.publish::<LibraryImported>(imported("/a", 1)).unwrap();
    nj.publish::<StepComputed>(7).unwrap(); // journaled, not relayable
    nj.publish_caused::<LibraryImported>(imported("/b", 2), a)
        .unwrap();

    let relay = Bus::relay(&*node).unwrap();
    let read = relay.resume(Cursor::START, 100);
    assert_eq!(read.lost, None);
    let kinds: Vec<(u64, &str, &str)> = read
        .entries
        .iter()
        .map(|e| (e.seq, e.kind.as_str(), e.text.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            (1, "library.imported", "/a|1"),
            (3, "library.imported", "/b|2")
        ]
    );
    assert_eq!(read.entries[1].cause, Some(1));
    assert_eq!(read.next, relay.cursor());

    for e in read.entries {
        Bus::relay(&*desktop)
            .unwrap()
            .ingest(&node_origin(), e)
            .unwrap();
    }
    let dj = Bus::journal(&*desktop).unwrap();
    assert!(dj.flush(WAIT));
    assert_eq!(
        *heard.lock().unwrap(),
        [imported("/a", 1), imported("/b", 2)]
    );
    let kept = dj.since(0, 10);
    assert_eq!(kept.entries[1].origin, Some(node_origin()));
    assert_eq!(kept.entries[1].cause, Some(1));
    // Taken in, not served again.
    assert!(
        Bus::relay(&*desktop)
            .unwrap()
            .resume(Cursor::START, 100)
            .entries
            .is_empty()
    );
    // A restarted node: another run.
    let restarted = full();
    assert!(declare_all(&*restarted, POINTS).is_empty());
    Bus::journal(&*restarted)
        .unwrap()
        .publish::<LibraryImported>(imported("/new", 1))
        .unwrap();
    let again = Bus::relay(&*restarted).unwrap().resume(read.next, 100);
    assert_eq!(again.lost, Some(Lost::Restarted));
    assert_eq!(again.entries.len(), 1);
}

#[test]
fn ingest_refuses_what_it_cannot_read_or_may_not_take() {
    let (_, desktop, heard) = node_and_desktop();
    let relay = Bus::relay(&*desktop).unwrap();
    relay
        .ingest(
            &node_origin(),
            RelayedEntry::new(1, "library.imported", "/a|4|new-field"),
        )
        .unwrap();
    assert_eq!(
        relay
            .ingest(
                &node_origin(),
                RelayedEntry::new(2, "library.imported", "/a")
            )
            .unwrap_err(),
        IngestError::Decode {
            kind: "library.imported".into(),
            reason: "no count".into()
        }
    );
    assert_eq!(
        relay
            .ingest(&node_origin(), RelayedEntry::new(3, "engine.step", "7"))
            .unwrap_err(),
        IngestError::NotRelayable {
            kind: "engine.step".into()
        }
    );
    // Unknown here: kept as text, heard by no one.
    assert_eq!(
        relay
            .ingest(&node_origin(), RelayedEntry::new(4, "extract.item", "{}"))
            .unwrap(),
        2
    );
    let journal = Bus::journal(&*desktop).unwrap();
    assert!(journal.flush(WAIT));
    assert_eq!(*heard.lock().unwrap(), [imported("/a", 4)]);
}

// --- Crossing: the kernel's own part ---

struct Delete;
impl Point for Delete {
    const ID: &'static str = "files.delete";
    const KIND: PointKind = PointKind::Request;
    type Input = String;
    type Value = Vec<String>;
}

impl Relayed for Delete {
    fn encode(input: &String) -> String {
        input.clone()
    }
    fn decode(text: &str) -> Result<String, String> {
        Ok(text.to_owned())
    }
}

impl RelayedRequest for Delete {
    fn encode_value(value: &Vec<String>) -> String {
        value.join(",")
    }
    fn decode_value(text: &str) -> Result<Vec<String>, String> {
        Ok(text
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

#[test]
fn a_repeat_of_an_ask_still_running_waits_for_its_first_answer() {
    let node = full();
    node.declare::<Delete>(
        PointSpec::new("Deletes.").relayable_request(Codec::request::<Delete>()),
    )
    .unwrap();
    let ran: Arc<Mutex<u32>> = Arc::default();
    let r = ran.clone();
    node.listen::<Delete>("node.slow", ListenOpts::new(), move |_, did| {
        thread::sleep(Duration::from_millis(100));
        *r.lock().unwrap() += 1;
        did.push("node.slow".into());
        Ok(())
    })
    .unwrap();
    let ask = RelayedAsk::new(1, 1, Delete::ID, WAIT, "/a", "");
    let from = Origin::process("desktop");
    let answers: Vec<RelayedAnswer> = thread::scope(|s| {
        let both: Vec<_> = (0..2)
            .map(|_| {
                let (node, ask, from) = (node.clone(), ask.clone(), from.clone());
                s.spawn(move || Bus::relay(&*node).unwrap().answer(&from, ask))
            })
            .collect();
        both.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(*ran.lock().unwrap(), 1, "run once");
    assert_eq!(answers[0], answers[1]);
    assert_eq!(
        answers[0].answer,
        Answer::Done {
            value: "node.slow".into()
        }
    );
}
