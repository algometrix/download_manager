//! The order downloads start in, which the user can rearrange.
//!
//! Every download has a position; lower starts first. Queue views show a
//! subset of the same order, so moving a download relative to its visible
//! neighbors keeps its place among downloads shown elsewhere.

use crate::record::DownloadId;

/// Where a moved download goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePlace {
    /// Just ahead of this download.
    Before(DownloadId),
    /// After every other download.
    End,
}

/// `order` with `moved` taken out and put at `place`. A place relative to
/// `moved` itself, or to a download not in `order`, leaves it unchanged.
pub fn reordered(order: &[DownloadId], moved: DownloadId, place: QueuePlace) -> Vec<DownloadId> {
    let unchanged = || order.to_vec();
    if !order.contains(&moved) {
        return unchanged();
    }
    let mut rest: Vec<DownloadId> = order.iter().copied().filter(|id| *id != moved).collect();
    let index = match place {
        QueuePlace::End => rest.len(),
        QueuePlace::Before(target) if target == moved => return unchanged(),
        QueuePlace::Before(target) => match rest.iter().position(|id| *id == target) {
            Some(index) => index,
            None => return unchanged(),
        },
    };
    rest.insert(index, moved);
    rest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[u64]) -> Vec<DownloadId> {
        values.iter().copied().map(DownloadId).collect()
    }

    #[test]
    fn moves_before_another_download() {
        let order = ids(&[1, 2, 3, 4]);
        assert_eq!(
            reordered(&order, DownloadId(4), QueuePlace::Before(DownloadId(1))),
            ids(&[4, 1, 2, 3])
        );
        assert_eq!(
            reordered(&order, DownloadId(1), QueuePlace::Before(DownloadId(3))),
            ids(&[2, 1, 3, 4])
        );
        assert_eq!(
            reordered(&order, DownloadId(2), QueuePlace::Before(DownloadId(3))),
            ids(&[1, 2, 3, 4])
        );
    }

    #[test]
    fn moves_to_the_end() {
        assert_eq!(
            reordered(&ids(&[1, 2, 3]), DownloadId(1), QueuePlace::End),
            ids(&[2, 3, 1])
        );
        assert_eq!(
            reordered(&ids(&[1, 2, 3]), DownloadId(3), QueuePlace::End),
            ids(&[1, 2, 3])
        );
    }

    #[test]
    fn nonsense_places_change_nothing() {
        let order = ids(&[1, 2, 3]);
        assert_eq!(
            reordered(&order, DownloadId(2), QueuePlace::Before(DownloadId(2))),
            order
        );
        assert_eq!(
            reordered(&order, DownloadId(2), QueuePlace::Before(DownloadId(9))),
            order
        );
        assert_eq!(reordered(&order, DownloadId(9), QueuePlace::End), order);
    }
}
