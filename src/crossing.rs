//! What crosses a process boundary (bus-contract's "Crossing"): the
//! kernel's authorities, the asks it answers once per origin, and the
//! checks it makes before handing an entry to the host's relay.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Condvar, Mutex, PoisonError, RwLock};
use std::time::Duration;

use bus_contract::{
    Answer, Authority, Bus, Crossing, IngestError, Origin, PointKind, RelayedAnswer, RelayedAsk,
    RelayedEntry,
};
use web_time::Instant;

use crate::Kernel;

/// How long an answer is kept at least, for a repeated ask to get it
/// again; longer when the ask's deadline is.
pub(crate) const KEEP_ANSWERS: Duration = Duration::from_secs(30);

/// An ask answered: who asked (origin process, run, number).
type AskKey = (String, u64, u64);

enum Kept {
    /// Its listeners are running: a repeat waits for the answer.
    Running,
    /// Its answer, kept until then.
    Done {
        answer: RelayedAnswer,
        until: Instant,
    },
}

#[derive(Default)]
struct Answered {
    asks: HashMap<AskKey, Kept>,
    /// Per origin process and run, the highest ask number whose answer
    /// was dropped: a repeat of one is refused, not run again.
    dropped: HashMap<(String, u64), u64>,
}

impl Answered {
    /// Drops the answers kept past their time.
    fn prune(&mut self, now: Instant) {
        let gone: Vec<AskKey> = self
            .asks
            .iter()
            .filter(|(_, k)| matches!(k, Kept::Done { until, .. } if *until <= now))
            .map(|(key, _)| key.clone())
            .collect();
        for (process, run, id) in gone {
            self.asks.remove(&(process.clone(), run, id));
            let high = self.dropped.entry((process, run)).or_insert(0);
            *high = (*high).max(id);
        }
    }
}

/// The kernel's side of crossing: its authorities and the asks it
/// answered.
#[derive(Default)]
pub(crate) struct Crossings {
    authorities: RwLock<Vec<Authority>>,
    answered: Mutex<Answered>,
    changed: Condvar,
}

impl Crossings {
    pub(crate) fn add_authority(&self, authority: Authority) {
        self.authorities
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(authority);
    }

    /// Every authority, in the order they were added; one refusal
    /// refuses, a panic refuses.
    pub(crate) fn authorize(&self, crossing: &Crossing<'_>) -> Result<(), String> {
        let all = self
            .authorities
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for authority in all {
            match catch_unwind(AssertUnwindSafe(|| authority(crossing))) {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => return Err(reason),
                Err(_) => return Err("an authority panicked".to_owned()),
            }
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Answered> {
        self.answered.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Kernel {
    /// [`Relay::ingest`](bus_contract::Relay::ingest): the kernel's checks,
    /// then the host's record.
    pub(crate) fn ingest_crossing(
        &self,
        origin: &Origin,
        entry: RelayedEntry,
    ) -> Result<u64, IngestError> {
        let Some(host) = &self.relay else {
            return Err(IngestError::NotRelayable { kind: entry.kind });
        };
        let crossing = Crossing::new(origin, &entry.kind, entry.version, false);
        if let Err(reason) = self.crossings.authorize(&crossing) {
            return Err(IngestError::Refused {
                kind: entry.kind,
                reason,
            });
        }
        if let Some((key, spec)) = self.declaration(&entry.kind) {
            if key.kind != PointKind::Event || spec.relay.is_none() {
                return Err(IngestError::NotRelayable { kind: entry.kind });
            }
            if spec.version != entry.version {
                return Err(IngestError::Version {
                    kind: entry.kind,
                    sent: entry.version,
                    here: spec.version,
                });
            }
        }
        host.ingest(origin, entry)
    }

    /// [`Relay::answer`](bus_contract::Relay::answer), once per origin.
    pub(crate) fn answer_crossing(&self, origin: &Origin, ask: RelayedAsk) -> RelayedAnswer {
        let got = Instant::now();
        let key: AskKey = (origin.process.clone(), ask.run, ask.id);
        {
            let mut answered = self.crossings.lock();
            answered.prune(got);
            loop {
                match answered.asks.get(&key) {
                    Some(Kept::Done { answer, .. }) => return answer.clone(),
                    Some(Kept::Running) => {
                        // A repeat while the first runs: its answer, if it
                        // comes within the ask's deadline.
                        let Some(left) = ask.deadline.checked_sub(got.elapsed()) else {
                            return RelayedAnswer::to(
                                &ask,
                                Answer::refused("the ask is still being answered"),
                            );
                        };
                        answered = self
                            .crossings
                            .changed
                            .wait_timeout(answered, left)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                    None => break,
                }
            }
            if answered
                .dropped
                .get(&(key.0.clone(), key.1))
                .is_some_and(|high| ask.id <= *high)
            {
                return RelayedAnswer::to(
                    &ask,
                    Answer::refused("the ask was answered already; its answer is no longer kept"),
                );
            }
            answered.asks.insert(key.clone(), Kept::Running);
        }
        // The listeners run with no lock held; a panic in them is the
        // kernel's to catch, never left as `Running`.
        let answer = catch_unwind(AssertUnwindSafe(|| self.answer_once(origin, &ask, got)))
            .unwrap_or_else(|_| Answer::refused("answering the ask panicked"));
        let answer = RelayedAnswer::to(&ask, answer);
        let mut answered = self.crossings.lock();
        answered.asks.insert(
            key,
            Kept::Done {
                answer: answer.clone(),
                until: got + ask.deadline.max(KEEP_ANSWERS),
            },
        );
        self.crossings.changed.notify_all();
        answer
    }

    fn answer_once(&self, origin: &Origin, ask: &RelayedAsk, got: Instant) -> Answer {
        let crossing = Crossing::new(origin, &ask.kind, ask.version, true);
        if let Err(reason) = self.crossings.authorize(&crossing) {
            return Answer::refused(reason);
        }
        let Some((key, spec)) = self.declaration(&ask.kind) else {
            return Answer::refused(format!("`{}` is not declared here", ask.kind));
        };
        let codec = spec
            .relay
            .filter(|c| key.kind == PointKind::Request && c.answers());
        let (Some(codec), Some(decode_value), Some(encode_value), Some(clone)) = (
            codec,
            codec.and_then(|c| c.decode_value),
            codec.and_then(|c| c.encode_value),
            codec.and_then(|c| c.clone_value),
        ) else {
            return Answer::refused(format!("`{}` is not a relayable request here", ask.kind));
        };
        if spec.version != ask.version {
            return Answer::refused(format!(
                "`{}`: asked at version {}, declared here at version {}",
                ask.kind, ask.version, spec.version
            ));
        }
        let input = match (codec.decode)(&ask.input) {
            Ok(input) => input,
            Err(reason) => return Answer::refused(format!("cannot read the input: {reason}")),
        };
        let mut value = (codec.fresh)();
        if let Err(reason) = decode_value(&ask.value, &mut *value) {
            return Answer::refused(format!("cannot read the value: {reason}"));
        }
        if got.elapsed() > ask.deadline {
            return Answer::refused("the deadline passed before the listeners ran");
        }
        let out = Bus::alter_any(self, key, input, value, clone);
        if got.elapsed() > ask.deadline {
            return Answer::Refused {
                listener: None,
                owner: None,
                reason: format!(
                    "the listeners ended past the deadline of {:?}",
                    ask.deadline
                ),
                failed: true,
            };
        }
        if let Some(r) = out.refused {
            return Answer::Refused {
                listener: Some(r.listener),
                owner: r.owner,
                reason: r.reason,
                failed: r.failed,
            };
        }
        match encode_value(&*out.value) {
            Some(value) => Answer::Done { value },
            None => Answer::refused("the value is not the point's"),
        }
    }
}
