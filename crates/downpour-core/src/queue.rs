//! Download queues: each runs up to its own number of downloads
//! at once and can be started or stopped. The Main queue always exists.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueueId(pub u32);

impl QueueId {
    pub const MAIN: QueueId = QueueId(1);
}

impl Default for QueueId {
    fn default() -> Self {
        QueueId::MAIN
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    /// Queued downloads start whenever the queue has a free slot.
    Running,
    /// Nothing in the queue starts until the queue is started again.
    Stopped,
}

pub const QUEUE_LIMIT_RANGE: std::ops::RangeInclusive<u8> = 1..=16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    pub id: QueueId,
    pub name: String,
    /// Downloads from this queue that may run at the same time.
    pub max_active: u8,
    pub state: QueueState,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    #[error("a queue needs a name")]
    EmptyName,
    #[error("a queue named {0:?} already exists")]
    DuplicateName(String),
    #[error("the Main queue cannot be removed")]
    MainIsPermanent,
    #[error("queue {0:?} does not exist")]
    Unknown(QueueId),
}

/// Every queue, with Main always first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<Queue>", into = "Vec<Queue>")]
pub struct Queues {
    list: Vec<Queue>,
}

impl Queues {
    pub fn with_main(max_active: u8) -> Self {
        Self {
            list: vec![main_queue(max_active)],
        }
    }

    pub fn all(&self) -> &[Queue] {
        &self.list
    }

    /// The queue with `id`, or Main when it was removed meanwhile.
    pub fn get(&self, id: QueueId) -> &Queue {
        self.list
            .iter()
            .find(|queue| queue.id == id)
            .unwrap_or(&self.list[0])
    }

    pub fn contains(&self, id: QueueId) -> bool {
        self.list.iter().any(|queue| queue.id == id)
    }

    pub fn add(&mut self, name: &str, max_active: u8) -> Result<QueueId, QueueError> {
        let name = self.checked_name(name, None)?;
        let id = QueueId(self.list.iter().map(|queue| queue.id.0).max().unwrap_or(0) + 1);
        self.list.push(Queue {
            id,
            name,
            max_active: clamp_limit(max_active),
            state: QueueState::Running,
        });
        Ok(id)
    }

    pub fn remove(&mut self, id: QueueId) -> Result<(), QueueError> {
        if id == QueueId::MAIN {
            return Err(QueueError::MainIsPermanent);
        }
        let before = self.list.len();
        self.list.retain(|queue| queue.id != id);
        if self.list.len() == before {
            return Err(QueueError::Unknown(id));
        }
        Ok(())
    }

    pub fn rename(&mut self, id: QueueId, name: &str) -> Result<(), QueueError> {
        let name = self.checked_name(name, Some(id))?;
        self.get_mut(id)?.name = name;
        Ok(())
    }

    pub fn set_limit(&mut self, id: QueueId, max_active: u8) -> Result<(), QueueError> {
        self.get_mut(id)?.max_active = clamp_limit(max_active);
        Ok(())
    }

    pub fn set_state(&mut self, id: QueueId, state: QueueState) -> Result<(), QueueError> {
        self.get_mut(id)?.state = state;
        Ok(())
    }

    fn get_mut(&mut self, id: QueueId) -> Result<&mut Queue, QueueError> {
        self.list
            .iter_mut()
            .find(|queue| queue.id == id)
            .ok_or(QueueError::Unknown(id))
    }

    fn checked_name(&self, name: &str, editing: Option<QueueId>) -> Result<String, QueueError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(QueueError::EmptyName);
        }
        let taken = self
            .list
            .iter()
            .any(|queue| Some(queue.id) != editing && queue.name.eq_ignore_ascii_case(name));
        if taken {
            return Err(QueueError::DuplicateName(name.to_owned()));
        }
        Ok(name.to_owned())
    }
}

fn main_queue(max_active: u8) -> Queue {
    Queue {
        id: QueueId::MAIN,
        name: "Main queue".to_owned(),
        max_active: clamp_limit(max_active),
        state: QueueState::Running,
    }
}

fn clamp_limit(max_active: u8) -> u8 {
    max_active.clamp(*QUEUE_LIMIT_RANGE.start(), *QUEUE_LIMIT_RANGE.end())
}

impl From<Vec<Queue>> for Queues {
    /// Accepts a saved list, restoring Main (first) if it is missing.
    fn from(mut list: Vec<Queue>) -> Self {
        list.retain(|queue| !queue.name.trim().is_empty());
        for queue in &mut list {
            queue.max_active = clamp_limit(queue.max_active);
        }
        match list.iter().position(|queue| queue.id == QueueId::MAIN) {
            Some(index) => {
                let main = list.remove(index);
                list.insert(0, main);
            }
            None => list.insert(0, main_queue(3)),
        }
        Self { list }
    }
}

impl From<Queues> for Vec<Queue> {
    fn from(queues: Queues) -> Self {
        queues.list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_queue_is_permanent_and_first() {
        let mut queues = Queues::with_main(3);
        let night = queues.add("Night", 2).unwrap();
        assert_eq!(
            queues.remove(QueueId::MAIN),
            Err(QueueError::MainIsPermanent)
        );
        assert_eq!(queues.all()[0].id, QueueId::MAIN);
        queues.remove(night).unwrap();
        assert_eq!(queues.get(night).id, QueueId::MAIN);
    }

    #[test]
    fn names_are_unique_and_limits_clamped() {
        let mut queues = Queues::with_main(3);
        assert_eq!(
            queues.add(" main QUEUE ", 1),
            Err(QueueError::DuplicateName("main QUEUE".into()))
        );
        let id = queues.add("Big files", 0).unwrap();
        assert_eq!(queues.get(id).max_active, 1);
        queues.set_limit(id, 200).unwrap();
        assert_eq!(queues.get(id).max_active, 16);
    }

    #[test]
    fn saved_list_without_main_gets_it_back_first() {
        let saved = r#"[{"id":4,"name":"Night","max_active":2,"state":"stopped"}]"#;
        let queues: Queues = serde_json::from_str(saved).unwrap();
        assert_eq!(queues.all()[0].id, QueueId::MAIN);
        assert_eq!(queues.get(QueueId(4)).state, QueueState::Stopped);
    }
}
