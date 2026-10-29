//! Byte-range bookkeeping for multi-connection downloads.
//!
//! The strategy mirrors the classic "dynamic segmentation" used by desktop
//! download managers: the first connection starts on the whole file, and every
//! additional connection takes the second half of whichever segment still has
//! the most unclaimed bytes. A connection that finishes early therefore helps
//! the slowest one instead of sitting idle, and no fixed split is ever wrong.
//!
//! A split is tentative until the server accepts the new connection: the
//! running connection keeps its whole range meanwhile. Servers that refuse
//! new requests late in a download (some download links do) would
//! otherwise leave the second half with nobody able to fetch it, while the
//! running connection could have finished it over the stream it has.

use std::time::Instant;

use serde::{Deserialize, Serialize};

/// A persisted byte range `[start, end)` of the target file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
    /// Bytes handed to the OS starting at `start`. Resume continues from here.
    pub written: u64,
}

impl Segment {
    pub fn write_position(&self) -> u64 {
        self.start + self.written
    }

    pub fn remaining(&self) -> u64 {
        self.end - self.write_position()
    }

    pub fn is_complete(&self) -> bool {
        self.write_position() >= self.end
    }
}

/// Stable handle for a segment; indices never shift when segments split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SlotId(usize);

/// Where a slot made by a split stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Split {
    /// A normal slot: its range is its own.
    Settled,
    /// The second half of `donor`, whose connection still owns the range
    /// until the new connection is accepted.
    Tentative { donor: usize },
    /// The new connection was refused or failed; the range stayed with the
    /// donor. The slot is kept so ids stay stable, and is ignored.
    Withdrawn,
}

/// What the new connection of a split does once the server accepted it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitConfirmation {
    /// Download the slot's range, first discarding this many bytes of the
    /// response: the donor claimed them while the request was on its way.
    Proceed { skip: u64 },
    /// The donor already claimed the whole range; close the connection.
    NothingLeft,
}

#[derive(Debug)]
struct Slot {
    segment: Segment,
    split: Split,
    /// Absolute offset up to which a connection has accepted bytes. Bytes in
    /// `write_position..claimed_to` are buffered in memory, not yet on disk.
    claimed_to: u64,
    assigned: bool,
    consecutive_failures: u32,
    retry_not_before: Option<Instant>,
}

impl Slot {
    fn new(segment: Segment) -> Self {
        Self {
            claimed_to: segment.write_position(),
            segment,
            split: Split::Settled,
            assigned: false,
            consecutive_failures: 0,
            retry_not_before: None,
        }
    }

    fn unclaimed(&self) -> u64 {
        self.segment.end.saturating_sub(self.claimed_to)
    }

    fn is_ready_for_retry(&self, now: Instant) -> bool {
        self.retry_not_before.is_none_or(|at| now >= at)
    }

    /// Whether the slot's range is its own (not tentative or withdrawn).
    fn is_settled(&self) -> bool {
        self.split == Split::Settled
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SegmentLayoutError {
    #[error("segments do not cover 0..{size} contiguously")]
    NotContiguous { size: u64 },
    #[error("segment {start}..{end} claims {written} written bytes")]
    WrittenPastEnd { start: u64, end: u64, written: u64 },
}

/// Live view of which bytes are written, in flight, or still unassigned.
#[derive(Debug)]
pub struct SegmentTable {
    slots: Vec<Slot>,
    size: u64,
}

impl SegmentTable {
    /// One segment covering the whole file, for the first connection.
    pub fn whole_file(size: u64) -> Self {
        Self {
            slots: vec![Slot::new(Segment {
                start: 0,
                end: size,
                written: 0,
            })],
            size,
        }
    }

    /// Rebuilds the table from persisted segments, rejecting layouts that
    /// would leave holes or overlaps in the file.
    pub fn from_saved(size: u64, mut segments: Vec<Segment>) -> Result<Self, SegmentLayoutError> {
        segments.sort_by_key(|segment| segment.start);
        let mut expected_start = 0;
        for segment in &segments {
            if segment.start != expected_start || segment.end < segment.start {
                return Err(SegmentLayoutError::NotContiguous { size });
            }
            if segment.write_position() > segment.end {
                return Err(SegmentLayoutError::WrittenPastEnd {
                    start: segment.start,
                    end: segment.end,
                    written: segment.written,
                });
            }
            expected_start = segment.end;
        }
        if expected_start != size {
            return Err(SegmentLayoutError::NotContiguous { size });
        }
        Ok(Self {
            slots: segments.into_iter().map(Slot::new).collect(),
            size,
        })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn written_bytes(&self) -> u64 {
        self.settled().map(|slot| slot.segment.written).sum()
    }

    pub fn is_complete(&self) -> bool {
        self.settled().all(|slot| slot.segment.is_complete())
    }

    pub fn assigned_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.assigned).count()
    }

    /// Slots that own their range; together they cover the file exactly.
    fn settled(&self) -> impl Iterator<Item = &Slot> {
        self.slots.iter().filter(|slot| slot.is_settled())
    }

    /// Segments ordered by file offset, for persistence and display.
    pub fn segments(&self) -> Vec<Segment> {
        let mut segments: Vec<Segment> = self.settled().map(|slot| slot.segment).collect();
        segments.sort_by_key(|segment| segment.start);
        segments
    }

    pub fn segment(&self, slot: SlotId) -> Segment {
        self.slots[slot.0].segment
    }

    /// Chooses work for one new connection: an idle unfinished segment first,
    /// otherwise the second half of the assigned segment with the most
    /// unclaimed bytes, as a tentative split (see [`Self::confirm_split`]).
    /// Returns `None` when splitting would create a segment smaller than
    /// `min_split`.
    pub fn assign_next(&mut self, now: Instant, min_split: u64) -> Option<SlotId> {
        if let Some(index) = self.slots.iter().position(|slot| {
            slot.is_settled()
                && !slot.assigned
                && !slot.segment.is_complete()
                && slot.is_ready_for_retry(now)
        }) {
            self.slots[index].assigned = true;
            return Some(SlotId(index));
        }

        // One tentative split per donor at a time: until it is accepted, the
        // donor still owns the range and must not be split again.
        let donors_waiting: Vec<usize> = self
            .slots
            .iter()
            .filter_map(|slot| match slot.split {
                Split::Tentative { donor } => Some(donor),
                Split::Settled | Split::Withdrawn => None,
            })
            .collect();
        let (donor, unclaimed) = self
            .slots
            .iter()
            .enumerate()
            .filter(|(index, slot)| {
                slot.assigned && slot.is_settled() && !donors_waiting.contains(index)
            })
            .map(|(index, slot)| (index, slot.unclaimed()))
            .max_by_key(|(_, unclaimed)| *unclaimed)?;
        if unclaimed < min_split.saturating_mul(2) {
            return None;
        }

        let donor_slot = &self.slots[donor];
        let split_at = donor_slot.claimed_to + unclaimed / 2;
        let mut tail = Slot::new(Segment {
            start: split_at,
            end: donor_slot.segment.end,
            written: 0,
        });
        tail.split = Split::Tentative { donor };
        tail.assigned = true;
        self.slots.push(tail);
        Some(SlotId(self.slots.len() - 1))
    }

    /// The server accepted the new connection of a split: the donor gives up
    /// the range it has not claimed yet. Settled slots proceed unchanged.
    pub fn confirm_split(&mut self, slot: SlotId) -> SplitConfirmation {
        let Split::Tentative { donor } = self.slots[slot.0].split else {
            return SplitConfirmation::Proceed { skip: 0 };
        };
        let requested_from = self.slots[slot.0].segment.start;
        let end = self.slots[slot.0].segment.end;
        let donor_claimed = self.slots[donor].claimed_to;
        if donor_claimed >= end {
            let tail = &mut self.slots[slot.0];
            tail.split = Split::Withdrawn;
            tail.assigned = false;
            return SplitConfirmation::NothingLeft;
        }
        let start = requested_from.max(donor_claimed);
        self.slots[donor].segment.end = start;
        let tail = &mut self.slots[slot.0];
        tail.segment.start = start;
        tail.claimed_to = start;
        tail.split = Split::Settled;
        SplitConfirmation::Proceed {
            skip: start - requested_from,
        }
    }

    /// Accepts up to `available` incoming bytes for `slot`, never past the
    /// segment's current end (which may have shrunk after a split). Returns
    /// how many of the bytes the connection may keep.
    pub fn claim(&mut self, slot: SlotId, available: u64) -> u64 {
        let slot = &mut self.slots[slot.0];
        let accepted = available.min(slot.unclaimed());
        slot.claimed_to += accepted;
        if accepted > 0 {
            slot.consecutive_failures = 0;
        }
        accepted
    }

    /// True once every byte of the segment has been claimed by its connection.
    pub fn is_fully_claimed(&self, slot: SlotId) -> bool {
        self.slots[slot.0].unclaimed() == 0
    }

    pub fn record_written(&mut self, slot: SlotId, bytes: u64) {
        let slot = &mut self.slots[slot.0];
        slot.segment.written += bytes;
        debug_assert!(slot.segment.write_position() <= slot.claimed_to);
    }

    /// Returns the segment to the pool after its connection ended. Claimed but
    /// unwritten bytes are forgotten so the next connection refetches them.
    pub fn release(&mut self, slot: SlotId) {
        let slot = &mut self.slots[slot.0];
        slot.assigned = false;
        if let Split::Tentative { .. } = slot.split {
            // Never accepted: the range stays with the donor.
            slot.split = Split::Withdrawn;
            return;
        }
        slot.claimed_to = slot.segment.write_position();
    }

    /// Releases the segment and delays its reassignment with exponential
    /// backoff. Returns the number of consecutive failures so far.
    pub fn release_after_failure(&mut self, slot: SlotId, now: Instant) -> u32 {
        self.release(slot);
        let slot = &mut self.slots[slot.0];
        if slot.split == Split::Withdrawn {
            // Nothing to retry: the donor still covers the range.
            return 0;
        }
        slot.consecutive_failures += 1;
        slot.retry_not_before = Some(now + retry_backoff(slot.consecutive_failures));
        slot.consecutive_failures
    }
}

/// 0.5 s, 1 s, 2 s, ... capped at 30 s.
fn retry_backoff(consecutive_failures: u32) -> std::time::Duration {
    let millis = 500u64 << consecutive_failures.saturating_sub(1).min(6);
    std::time::Duration::from_millis(millis.min(30_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    /// Starts the first connection and splits off a second, returning both.
    fn split_two(size: u64) -> (SegmentTable, SlotId, SlotId) {
        let mut table = SegmentTable::whole_file(size);
        let first = table.assign_next(Instant::now(), MIB).unwrap();
        let second = table.assign_next(Instant::now(), MIB).unwrap();
        (table, first, second)
    }

    #[test]
    fn a_split_waits_for_the_new_connection_to_be_accepted() {
        let (mut table, first, second) = split_two(100 * MIB);
        // Until accepted, the first connection still owns the whole file.
        assert_eq!(table.segment(first).end, 100 * MIB);
        assert_eq!(table.segments().len(), 1);
        assert_eq!(
            table.confirm_split(second),
            SplitConfirmation::Proceed { skip: 0 }
        );
        assert_eq!(table.segment(first).end, 50 * MIB);
        assert_eq!(table.segment(second).start, 50 * MIB);
        assert_eq!(table.segments().len(), 2);
    }

    #[test]
    fn a_refused_split_leaves_the_range_with_the_running_connection() {
        let (mut table, first, second) = split_two(100 * MIB);
        assert_eq!(table.release_after_failure(second, Instant::now()), 0);
        assert_eq!(table.segment(first).end, 100 * MIB);
        // The running connection can still claim every byte.
        assert_eq!(table.claim(first, 100 * MIB), 100 * MIB);
        table.record_written(first, 100 * MIB);
        assert!(table.is_complete());
        // The withdrawn half is never handed out again.
        assert!(table.assign_next(Instant::now(), MIB).is_none());
    }

    #[test]
    fn bytes_the_donor_claimed_meanwhile_are_skipped() {
        let (mut table, first, second) = split_two(100 * MIB);
        table.claim(first, 60 * MIB);
        assert_eq!(
            table.confirm_split(second),
            SplitConfirmation::Proceed { skip: 10 * MIB }
        );
        assert_eq!(table.segment(first).end, 60 * MIB);
        assert_eq!(table.segment(second).start, 60 * MIB);
    }

    #[test]
    fn a_split_the_donor_finished_has_nothing_left() {
        let (mut table, first, second) = split_two(100 * MIB);
        table.claim(first, 100 * MIB);
        assert_eq!(table.confirm_split(second), SplitConfirmation::NothingLeft);
        assert_eq!(table.segment(first).end, 100 * MIB);
    }

    #[test]
    fn first_connection_owns_whole_file() {
        let mut table = SegmentTable::whole_file(100 * MIB);
        let first = table.assign_next(Instant::now(), MIB).unwrap();
        assert_eq!(
            table.segment(first),
            Segment {
                start: 0,
                end: 100 * MIB,
                written: 0
            }
        );
    }

    #[test]
    fn new_connection_splits_largest_unclaimed_segment_in_half() {
        let now = Instant::now();
        let mut table = SegmentTable::whole_file(100 * MIB);
        let first = table.assign_next(now, MIB).unwrap();
        table.claim(first, 20 * MIB);

        let second = table.assign_next(now, MIB).unwrap();
        table.confirm_split(second);

        assert_eq!(table.segment(first).end, 60 * MIB);
        assert_eq!(
            table.segment(second),
            Segment {
                start: 60 * MIB,
                end: 100 * MIB,
                written: 0
            }
        );
        assert_eq!(
            table
                .segments()
                .iter()
                .map(|s| s.end - s.start)
                .sum::<u64>(),
            100 * MIB
        );
    }

    #[test]
    fn claim_stops_at_shrunk_end() {
        let now = Instant::now();
        let mut table = SegmentTable::whole_file(10 * MIB);
        let first = table.assign_next(now, MIB).unwrap();
        let second = table.assign_next(now, MIB).unwrap();
        table.confirm_split(second);

        assert_eq!(table.claim(first, 8 * MIB), 5 * MIB);
        assert!(table.is_fully_claimed(first));
    }

    #[test]
    fn refuses_splits_below_minimum() {
        let now = Instant::now();
        let mut table = SegmentTable::whole_file(MIB);
        table.assign_next(now, MIB).unwrap();
        assert_eq!(table.assign_next(now, MIB), None);
    }

    #[test]
    fn idle_unfinished_segment_is_reused_before_splitting() {
        let now = Instant::now();
        let mut table = SegmentTable::whole_file(10 * MIB);
        let first = table.assign_next(now, MIB).unwrap();
        table.claim(first, MIB);
        table.record_written(first, MIB);
        table.release(first);

        assert_eq!(table.assign_next(now, MIB), Some(first));
        assert_eq!(table.segment(first).write_position(), MIB);
    }

    #[test]
    fn failed_segment_waits_for_backoff() {
        let now = Instant::now();
        let mut table = SegmentTable::whole_file(MIB);
        let first = table.assign_next(now, MIB).unwrap();
        assert_eq!(table.release_after_failure(first, now), 1);

        assert_eq!(table.assign_next(now, MIB), None);
        assert_eq!(
            table.assign_next(now + std::time::Duration::from_millis(600), MIB),
            Some(first)
        );
    }

    #[test]
    fn retry_backoff_grows_and_caps() {
        assert_eq!(retry_backoff(1).as_millis(), 500);
        assert_eq!(retry_backoff(3).as_millis(), 2000);
        assert_eq!(retry_backoff(20).as_millis(), 30_000);
    }

    #[test]
    fn release_forgets_unwritten_claims() {
        let now = Instant::now();
        let mut table = SegmentTable::whole_file(10 * MIB);
        let first = table.assign_next(now, MIB).unwrap();
        table.claim(first, 4 * MIB);
        table.record_written(first, MIB);
        table.release(first);

        let again = table.assign_next(now, MIB).unwrap();
        assert_eq!(table.claim(again, 100 * MIB), 9 * MIB);
    }

    #[test]
    fn saved_layout_must_be_contiguous() {
        let gap = vec![
            Segment {
                start: 0,
                end: 10,
                written: 10,
            },
            Segment {
                start: 12,
                end: 20,
                written: 0,
            },
        ];
        assert_eq!(
            SegmentTable::from_saved(20, gap).unwrap_err(),
            SegmentLayoutError::NotContiguous { size: 20 }
        );

        let valid = vec![
            Segment {
                start: 10,
                end: 20,
                written: 3,
            },
            Segment {
                start: 0,
                end: 10,
                written: 10,
            },
        ];
        let table = SegmentTable::from_saved(20, valid).unwrap();
        assert_eq!(table.written_bytes(), 13);
        assert!(!table.is_complete());
    }
}
