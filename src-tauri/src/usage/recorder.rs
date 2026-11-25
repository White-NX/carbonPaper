//! Merge observations in memory; checkpoints update an existing interval.
use super::types::{UsageIdentity, UsageSpan, UsageState};
use std::{collections::VecDeque, sync::Arc};

pub(super) const MAX_OBSERVATION_GAP_MS: i64 = 15_000;
const MAX_PENDING_SPANS: usize = 256;

pub(super) struct Recorder {
    run_id: i64,
    next_sequence: i64,
    current: Option<UsageSpan>,
    pending: VecDeque<UsageSpan>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            run_id: (rand::random::<u64>() & i64::MAX as u64) as i64,
            next_sequence: 0,
            current: None,
            pending: VecDeque::new(),
        }
    }

    /// Returns true on an interval boundary so the writer can checkpoint it.
    pub fn observe(
        &mut self,
        now_ms: i64,
        state: UsageState,
        identity: Option<Arc<UsageIdentity>>,
        idle_boundary_ms: Option<i64>,
    ) -> bool {
        let identity = (state == UsageState::Active).then_some(identity).flatten();
        let mut changed = false;
        if let Some(current) = self.current.as_ref() {
            let gap = now_ms - current.end_ms;
            if !(0..=MAX_OBSERVATION_GAP_MS).contains(&gap) {
                // A suspended process or changed wall clock supplies no evidence
                // for the missing period. Keep the last observed end unchanged.
                self.finish();
                changed = true;
            }
        }
        if let Some(current) = self.current.as_mut() {
            if current.state == state && current.identity == identity {
                current.end_ms = now_ms;
                return changed;
            }
            let boundary = if state == UsageState::Idle && current.state == UsageState::Active {
                idle_boundary_ms
                    .unwrap_or(now_ms)
                    .clamp(current.end_ms, now_ms)
            } else {
                now_ms
            };
            current.end_ms = boundary;
            self.finish();
            self.begin(boundary, now_ms, state, identity);
        } else {
            self.begin(now_ms, now_ms, state, identity);
        }
        true
    }

    fn begin(
        &mut self,
        start_ms: i64,
        end_ms: i64,
        state: UsageState,
        identity: Option<Arc<UsageIdentity>>,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.current = Some(UsageSpan {
            run_id: self.run_id,
            sequence,
            start_ms,
            end_ms,
            state,
            identity,
        });
    }

    pub fn finish(&mut self) {
        if let Some(span) = self
            .current
            .take()
            .filter(|span| span.end_ms > span.start_ms)
        {
            self.pending.push_back(span);
        }
        // A failed archive cannot retain an unbounded queue of private titles.
        // Dropped, uncommitted observations appear as unknown gaps on reads.
        while self.pending.len() > MAX_PENDING_SPANS {
            self.pending.pop_front();
        }
    }

    pub fn snapshot(&self) -> Vec<UsageSpan> {
        self.pending
            .iter()
            .cloned()
            .chain(self.current.clone())
            .collect()
    }

    pub fn committed(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_starts_at_the_threshold_and_checkpoints_keep_the_same_identity() {
        // Use regular observations so this is an idle transition, not suspension.
        let mut recorder = Recorder::new();
        recorder.observe(0, UsageState::Active, None, None);
        for time in (1_000..180_000).step_by(1_000) {
            recorder.observe(time, UsageState::Active, None, None);
        }
        let checkpoint = recorder.snapshot().pop().unwrap();
        recorder.committed();
        recorder.observe(180_700, UsageState::Idle, None, Some(180_000));
        let spans = recorder.snapshot();
        assert_eq!(spans[0].sequence, checkpoint.sequence);
        assert_eq!(spans[0].end_ms, 180_000);
        assert_eq!(spans[1].start_ms, 180_000);
        assert_eq!(spans[1].state, UsageState::Idle);
    }

    #[test]
    fn a_suspended_loop_does_not_charge_the_unobserved_time() {
        let mut recorder = Recorder::new();
        recorder.observe(0, UsageState::Active, None, None);
        recorder.observe(1_000, UsageState::Active, None, None);
        recorder.observe(3_600_000, UsageState::Active, None, None);
        let spans = recorder.snapshot();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].end_ms - spans[0].start_ms, 1_000);
        assert_eq!(spans[1].start_ms, 3_600_000);
        assert_eq!(spans[1].end_ms - spans[1].start_ms, 0);
    }
}
