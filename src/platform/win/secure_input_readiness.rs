//! Passive acknowledgment of an actually completed input-context refresh.
//! Compiled only for the hosted debug fixture; no text or provider calls are
//! exposed by the inspection message.

use super::{
    IdentityPublications, MetadataIdentity, PUBLISHED_IDENTITIES, Probe, RwLock, Status, Verdict,
    capture_generation, generation, identity_matches, validate_capture_generation,
    validate_generation,
};

#[derive(Clone, Copy)]
struct Acknowledgment {
    probe: Probe,
    context_generation: u64,
    identity: MetadataIdentity,
}

static COMPLETED_REFRESH: RwLock<Option<Acknowledgment>> = RwLock::new(None);

fn update_acknowledgment(
    latest: &mut Option<Acknowledgment>,
    request: Probe,
    acknowledgment: Option<Acknowledgment>,
) {
    // A callback can enqueue its old epoch after navigation already queued a
    // new refresh. Its later completion must not erase the new acknowledgment.
    if latest.is_some_and(|current| current.probe.generation > request.generation) {
        return;
    }
    if acknowledgment.is_some()
        || latest.is_some_and(|current| current.probe.target == request.target)
    {
        *latest = acknowledgment;
    }
}

impl Acknowledgment {
    fn from_completion(
        probe: Probe,
        result: Verdict,
        status: Status,
        context_generation: u64,
        still_current: impl FnOnce() -> bool,
    ) -> Option<Self> {
        (status.is_allowed()
            && result.status.is_allowed()
            && !matches!(result.identity, MetadataIdentity::Unavailable)
            && still_current())
        .then_some(Self {
            probe,
            context_generation,
            identity: result.identity,
        })
    }

    fn observed_handles(
        self,
        raw_generation: u64,
        context_generation: u64,
        publications: &IdentityPublications,
    ) -> Option<(usize, usize)> {
        let identity = publications
            .admission(self.probe.target, publications.frontier())
            .ok();
        (self.probe.generation == raw_generation
            && self.context_generation == context_generation
            && identity_matches(identity, self.identity))
        .then_some((self.probe.target.foreground, self.probe.target.focus))
    }
}

/// Only the real refresh coordinator publishes this observation, after both
/// provider acceptance and journal ownership handling have completed.
pub(super) fn record_completion(probe: Probe, result: Verdict, status: Status) {
    let context_generation = generation();
    let acknowledgment =
        Acknowledgment::from_completion(probe, result, status, context_generation, || {
            validate_capture_generation(probe.generation)
                && probe.target.is_current()
                && validate_capture_generation(probe.generation)
                && validate_generation(context_generation)
        });
    let mut latest = COMPLETED_REFRESH
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    update_acknowledgment(&mut latest, probe, acknowledgment);
}

/// Read cached evidence only: do not inspect native/UIA focus, enqueue work,
/// drain a worker or change the journal. The harness compares these handles
/// against its independently observed foreground and focused native window.
pub(in crate::platform::win) fn observed_handles() -> Option<(usize, usize)> {
    let acknowledgment = (*COMPLETED_REFRESH.try_read().ok()?)?;
    let raw_generation = capture_generation();
    let context_generation = generation();
    let handles = acknowledgment.observed_handles(
        raw_generation,
        context_generation,
        &PUBLISHED_IDENTITIES,
    )?;
    (validate_capture_generation(raw_generation) && validate_generation(context_generation))
        .then_some(handles)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::platform::win::secure_input::Target;

    #[test]
    fn fixture_acknowledgment_requires_completed_current_refresh_and_identity() {
        let publications = IdentityPublications::new();
        let probe = Probe {
            target: Target {
                foreground: 10,
                focus: 20,
                thread: 30,
                process: 40,
            },
            generation: 7,
        };
        let revision = publications.publish(probe.target, true, false);
        let result = Verdict {
            status: Status::Verified,
            element_changed: true,
            identity: MetadataIdentity::Published(revision),
        };
        let completed =
            Acknowledgment::from_completion(probe, result, Status::Verified, 11, || true)
                .expect("a completed accepted refresh can acknowledge its new owner");
        assert_eq!(
            completed.observed_handles(7, 11, &publications),
            Some((10, 20))
        );
        assert!(completed.observed_handles(8, 11, &publications).is_none());
        assert!(completed.observed_handles(7, 12, &publications).is_none());

        assert!(
            Acknowledgment::from_completion(probe, result, Status::Unknown, 11, || true).is_none()
        );
        assert!(
            Acknowledgment::from_completion(probe, Verdict::UNKNOWN, Status::Verified, 11, || true)
                .is_none()
        );
        assert!(
            Acknowledgment::from_completion(probe, result, Status::Verified, 11, || false)
                .is_none()
        );

        // A provisional publication for another DOM field sharing this HWND
        // cannot reuse the completed acknowledgment, even before epoch advance.
        publications.publish(probe.target, true, false);
        assert!(completed.observed_handles(7, 11, &publications).is_none());
    }

    #[test]
    fn stale_refresh_completion_cannot_erase_a_newer_acknowledgment() {
        let publications = IdentityPublications::new();
        let old = Probe {
            target: Target {
                foreground: 10,
                focus: 20,
                thread: 30,
                process: 40,
            },
            generation: 7,
        };
        let current = Probe {
            generation: 8,
            ..old
        };
        let revision = publications.publish(current.target, true, false);
        let result = Verdict {
            status: Status::Verified,
            element_changed: false,
            identity: MetadataIdentity::Published(revision),
        };
        let mut latest = None;
        let accepted =
            Acknowledgment::from_completion(current, result, Status::Verified, 12, || true);
        update_acknowledgment(&mut latest, current, accepted);
        // The new reset's real completion is stored before an older callback's
        // queued refresh reaches the coordinator and reports Unknown.
        update_acknowledgment(&mut latest, old, None);
        assert_eq!(
            latest.and_then(|ack| ack.observed_handles(8, 12, &publications)),
            Some((10, 20))
        );
        update_acknowledgment(&mut latest, current, None);
        assert!(
            latest.is_none(),
            "a current denied refresh must withdraw its acknowledgment"
        );
    }
}
