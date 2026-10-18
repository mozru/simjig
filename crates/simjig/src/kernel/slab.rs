//! A generational slab: dense storage addressed by [`Key`]s that go stale when their entry
//! is removed.
//!
//! Free slots are reused last-in first-out, so the sequence of keys handed out depends only
//! on the sequence of inserts and removes. That keeps every key (and the trace that records
//! them) deterministic.

use super::OrInvariant;

/// Index plus generation; a key whose generation no longer matches its slot is stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Key {
    pub(crate) index: u32,
    pub(crate) gen_: u32,
}

#[derive(Debug)]
struct Slot<T> {
    gen_: u32,
    value: Option<T>,
}

#[derive(Debug)]
pub(crate) struct Slab<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    len: usize,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Slab<T> {
    pub(crate) fn insert(&mut self, value: T) -> Key {
        self.len += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.gen_ = slot.gen_.wrapping_add(1);
            slot.value = Some(value);
            Key {
                index,
                gen_: slot.gen_,
            }
        } else {
            let index = u32::try_from(self.slots.len()).or_invariant("invariant: < 2^32 entries");
            self.slots.push(Slot {
                gen_: 0,
                value: Some(value),
            });
            Key { index, gen_: 0 }
        }
    }

    pub(crate) fn remove(&mut self, key: Key) -> Option<T> {
        let slot = self.slots.get_mut(key.index as usize)?;
        if slot.gen_ != key.gen_ {
            return None;
        }
        let value = slot.value.take()?;
        self.free.push(key.index);
        self.len -= 1;
        Some(value)
    }

    pub(crate) fn get(&self, key: Key) -> Option<&T> {
        let slot = self.slots.get(key.index as usize)?;
        if slot.gen_ == key.gen_ {
            slot.value.as_ref()
        } else {
            None
        }
    }

    pub(crate) fn get_mut(&mut self, key: Key) -> Option<&mut T> {
        let slot = self.slots.get_mut(key.index as usize)?;
        if slot.gen_ == key.gen_ {
            slot.value.as_mut()
        } else {
            None
        }
    }

    /// The key of the live entry at `index`, if any.
    pub(crate) fn key_at(&self, index: u32) -> Option<Key> {
        let slot = self.slots.get(index as usize)?;
        slot.value.as_ref().map(|_| Key {
            index,
            gen_: slot.gen_,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Live entries in index order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Key, &T)> {
        self.slots.iter().zip(0u32..).filter_map(|(slot, index)| {
            slot.value.as_ref().map(|v| {
                (
                    Key {
                        index,
                        gen_: slot.gen_,
                    },
                    v,
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_keys_go_stale_and_slots_are_reused() {
        let mut slab = Slab::default();
        let a = slab.insert('a');
        slab.insert('b');
        assert_eq!(slab.remove(a), Some('a'));
        assert_eq!(slab.get(a), None);
        let c = slab.insert('c');
        assert_eq!(c.index, a.index);
        assert_ne!(c, a);
        assert_eq!(slab.remove(a), None);
        assert_eq!(slab.len(), 2);
        assert_eq!(slab.iter().map(|(_, v)| *v).collect::<String>(), "cb");
    }
}
