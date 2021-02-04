use crate::arena_backing::{AnonMmap, ArenaBacking};
use crate::{Hash256, SmallVec8};
use ssz_derive::{Decode, Encode};
use std::cmp::Ordering;
use std::ops::Range;

#[derive(Debug, PartialEq, Clone)]
pub enum Error {
    UnknownAllocId(usize),
    OffsetOverflow,
    OffsetUnderflow,
    RangeOverFlow,
}

pub type CacheArena = GenericCacheArena<AnonMmap>;
// pub type CacheArena = GenericCacheArena<Vec<Hash256>>;

/// Inspired by the `TypedArena` crate, the `CachedArena` provides a single contiguous memory
/// allocation from which smaller allocations can be produced. In effect this allows for having
/// many `Vec<T>`-like objects all stored contiguously on the heap with the aim of reducing memory
/// fragmentation.
///
/// Because all of the allocations are stored in one big `Vec`, resizing any of the allocations
/// will mean all items to the right of that allocation will be moved.
#[derive(Debug, PartialEq, Clone, Default, Encode, Decode)]
pub struct GenericCacheArena<B: ArenaBacking> {
    /// The backing array, storing cached values.
    backing: B,
    /// A list of offsets indicating the start of each allocation.
    offsets: Vec<usize>,
}

impl<B: ArenaBacking> GenericCacheArena<B> {
    /// Instantiate self with a backing array of the given `capacity`.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            backing: B::with_capacity(capacity),
            offsets: vec![],
        }
    }

    /// Produce an allocation of zero length at the end of the backing array.
    pub fn alloc(&mut self) -> CacheArenaAllocation {
        let alloc_id = self.offsets.len();
        self.offsets.push(self.backing.len());

        CacheArenaAllocation { alloc_id }
    }

    /// Update `self.offsets` to reflect an allocation increasing in size.
    fn grow(&mut self, alloc_id: usize, grow_by: usize) -> Result<(), Error> {
        if alloc_id < self.offsets.len() {
            self.offsets
                .iter_mut()
                .skip(alloc_id + 1)
                .try_for_each(|offset| {
                    *offset = offset.checked_add(grow_by).ok_or(Error::OffsetOverflow)?;

                    Ok(())
                })
        } else {
            Err(Error::UnknownAllocId(alloc_id))
        }
    }

    /// Update `self.offsets` to reflect an allocation decreasing in size.
    fn shrink(&mut self, alloc_id: usize, shrink_by: usize) -> Result<(), Error> {
        if alloc_id < self.offsets.len() {
            self.offsets
                .iter_mut()
                .skip(alloc_id + 1)
                .try_for_each(|offset| {
                    *offset = offset
                        .checked_sub(shrink_by)
                        .ok_or(Error::OffsetUnderflow)?;

                    Ok(())
                })
        } else {
            Err(Error::UnknownAllocId(alloc_id))
        }
    }

    /// Similar to `Vec::splice`, however the range is relative to some allocation (`alloc_id`) and
    /// the replaced items are not returned (i.e., it is forgetful).
    ///
    /// To reiterate, the given `range` should be relative to the given `alloc_id`, not
    /// `self.backing`. E.g., if the allocation has an offset of `20` and the range is `0..1`, then
    /// the splice will translate to `self.backing[20..21]`.
    fn splice_forgetful(
        &mut self,
        alloc_id: usize,
        range: Range<usize>,
        replace_with: &[Hash256],
    ) -> Result<(), Error> {
        let offset = self
            .offsets
            .get(alloc_id)
            .copied()
            .ok_or(Error::UnknownAllocId(alloc_id))?;
        let start = range
            .start
            .checked_add(offset)
            .ok_or(Error::RangeOverFlow)?;
        let end = range.end.checked_add(offset).ok_or(Error::RangeOverFlow)?;

        let prev_len = self.backing.len();

        self.backing.splice_forgetful(start..end, replace_with);

        match prev_len.cmp(&self.backing.len()) {
            Ordering::Greater => self.shrink(alloc_id, prev_len - self.backing.len())?,
            Ordering::Less => self.grow(alloc_id, self.backing.len() - prev_len)?,
            Ordering::Equal => {}
        }

        Ok(())
    }

    /// Returns the length of the specified allocation.
    fn len(&self, alloc_id: usize) -> Result<usize, Error> {
        let start = self
            .offsets
            .get(alloc_id)
            .ok_or(Error::UnknownAllocId(alloc_id))?;
        let end = self
            .offsets
            .get(alloc_id + 1)
            .copied()
            .unwrap_or_else(|| self.backing.len());

        Ok(end - start)
    }

    /// Get the value at position `i`, relative to the offset at `alloc_id`.
    fn get(&self, alloc_id: usize, i: usize) -> Result<Option<Hash256>, Error> {
        if i < self.len(alloc_id)? {
            let offset = self
                .offsets
                .get(alloc_id)
                .ok_or(Error::UnknownAllocId(alloc_id))?;
            Ok(self.backing.get(i + offset))
        } else {
            Ok(None)
        }
    }

    /// Mutably get the value at position `i`, relative to the offset at `alloc_id`.
    fn get_mut(&mut self, alloc_id: usize, i: usize) -> Result<Option<&mut [u8]>, Error> {
        if i < self.len(alloc_id)? {
            let offset = self
                .offsets
                .get(alloc_id)
                .ok_or(Error::UnknownAllocId(alloc_id))?;
            Ok(self.backing.get_mut(i + offset))
        } else {
            Ok(None)
        }
    }

    /// Returns the range in `self.backing` that is occupied by some allocation.
    fn range(&self, alloc_id: usize) -> Result<Range<usize>, Error> {
        let start = self
            .offsets
            .get(alloc_id)
            .copied()
            .ok_or(Error::UnknownAllocId(alloc_id))?;
        let end = self
            .offsets
            .get(alloc_id + 1)
            .copied()
            .unwrap_or_else(|| self.backing.len());

        Ok(start..end)
    }

    /// Iterate through all values in some allocation.
    fn iter(&self, alloc_id: usize) -> Result<impl Iterator<Item = Hash256> + '_, Error> {
        Ok(self.backing.iter_range(self.range(alloc_id)?))
    }

    /// Mutably iterate through all values in some allocation.
    fn iter_mut(&mut self, alloc_id: usize) -> Result<impl Iterator<Item = &mut [u8]>, Error> {
        let range = self.range(alloc_id)?;
        Ok(self.backing.iter_range_mut(range))
    }

    /// Returns the total number of items stored in the arena, the sum of all values in all
    /// allocations.
    pub fn backing_len(&self) -> usize {
        self.backing.len()
    }

    pub fn extend_capacity(&mut self, capacity: usize) {
        self.backing.extend_capacity(capacity)
    }
}

/// An allocation from a `CacheArena` that behaves like a `Vec<T>`.
///
/// All functions will modify the given `arena` instead of `self`. As such, it is safe to have
/// multiple instances of this allocation at once.
///
/// For all functions that accept a `CacheArena<T>` parameter, that arena should always be the one
/// that created `Self`. I.e., do not mix-and-match allocations and arenas unless you _really_ know
/// what you're doing (or want to have a bad time).
#[derive(Debug, PartialEq, Clone, Default, Encode, Decode)]
pub struct CacheArenaAllocation {
    alloc_id: usize,
}

impl CacheArenaAllocation {
    /// Grow the allocation in `arena`, appending `vec` to the current values.
    pub fn extend_with_vec(
        &self,
        arena: &mut CacheArena,
        vec: SmallVec8<Hash256>,
    ) -> Result<(), Error> {
        let len = arena.len(self.alloc_id)?;
        arena.splice_forgetful(self.alloc_id, len..len, &vec)?;
        Ok(())
    }

    /// Push `item` to the end of the current allocation in `arena`.
    ///
    /// An error is returned if this allocation is not known to the given `arena`.
    pub fn push(&self, arena: &mut CacheArena, item: Hash256) -> Result<(), Error> {
        let len = arena.len(self.alloc_id)?;
        arena.splice_forgetful(self.alloc_id, len..len, &[item])?;
        Ok(())
    }

    /// Get the i'th item in the `arena` (relative to this allocation).
    ///
    /// An error is returned if this allocation is not known to the given `arena`.
    pub fn get(&self, arena: &CacheArena, i: usize) -> Result<Option<Hash256>, Error> {
        arena.get(self.alloc_id, i)
    }

    /// Mutably get the i'th item in the `arena` (relative to this allocation).
    ///
    /// An error is returned if this allocation is not known to the given `arena`.
    pub fn get_mut<'a>(
        &self,
        arena: &'a mut CacheArena,
        i: usize,
    ) -> Result<Option<&'a mut [u8]>, Error> {
        arena.get_mut(self.alloc_id, i)
    }

    /// Iterate through all items in the `arena` (relative to this allocation).
    pub fn iter<'a>(
        &self,
        arena: &'a CacheArena,
    ) -> Result<impl Iterator<Item = Hash256> + 'a, Error> {
        arena.iter(self.alloc_id)
    }

    /// Mutably iterate through all items in the `arena` (relative to this allocation).
    pub fn iter_mut<'a>(
        &self,
        arena: &'a mut CacheArena,
    ) -> Result<impl Iterator<Item = &'a mut [u8]>, Error> {
        arena.iter_mut(self.alloc_id)
    }

    /// Return the number of items stored in this allocation.
    pub fn len(&self, arena: &CacheArena) -> Result<usize, Error> {
        arena.len(self.alloc_id)
    }

    /// Returns true if this allocation is empty.
    pub fn is_empty(&self, arena: &CacheArena) -> Result<bool, Error> {
        self.len(arena).map(|len| len == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Hash256;
    use smallvec::smallvec;

    fn hash(i: usize) -> Hash256 {
        Hash256::from_low_u64_be(i as u64)
    }

    fn test_routine(arena: &mut CacheArena, sub: &mut CacheArenaAllocation) {
        let mut len = sub.len(arena).expect("should exist");

        sub.push(arena, hash(len)).expect("should push");
        len += 1;

        assert_eq!(
            sub.len(arena).expect("should exist"),
            len,
            "after first push sub should have len {}",
            len
        );
        assert_eq!(
            sub.is_empty(arena).expect("should exist"),
            false,
            "new sub should not be empty"
        );

        sub.push(arena, hash(len)).expect("should push again");
        len += 1;

        assert_eq!(
            sub.len(arena).expect("should exist"),
            len,
            "after second push sub should have len {}",
            len
        );

        sub.extend_with_vec(arena, smallvec![hash(len), hash(len + 1)])
            .expect("should extend with vec");
        len += 2;

        assert_eq!(
            sub.len(arena).expect("should exist"),
            len,
            "after extend sub should have len {}",
            len
        );

        let collected = sub
            .iter(arena)
            .expect("should get iter")
            .collect::<Vec<_>>();
        let collected_mut = sub
            .iter_mut(arena)
            .expect("should get mut iter")
            .map(|slice| Hash256::from_slice(slice))
            .collect::<Vec<_>>();

        for i in 0..len {
            assert_eq!(
                sub.get(arena, i)
                    .expect("should exist")
                    .expect("should get sub index"),
                hash(i),
                "get({}) should be hash({})",
                i,
                i
            );

            assert_eq!(
                collected[i],
                hash(i),
                "collected[{}] should be hash({})",
                i,
                i
            );

            assert_eq!(
                collected_mut[i],
                hash(i),
                "collected_mut[{}] should be hash({})",
                i,
                i
            );
        }
    }

    #[test]
    fn single() {
        let arena = &mut CacheArena::default();

        assert_eq!(arena.backing.len(), 0, "should start with an empty backing");
        assert_eq!(arena.offsets.len(), 0, "should start without any offsets");

        let mut sub = arena.alloc();

        assert_eq!(
            sub.len(arena).expect("should exist"),
            0,
            "new sub should have len 0"
        );
        assert_eq!(
            sub.is_empty(arena).expect("should exist"),
            true,
            "new sub should be empty"
        );

        test_routine(arena, &mut sub);
    }

    #[test]
    fn double() {
        let arena = &mut CacheArena::default();

        assert_eq!(arena.backing.len(), 0, "should start with an empty backing");
        assert_eq!(arena.offsets.len(), 0, "should start without any offsets");

        let mut sub_01 = arena.alloc();
        assert_eq!(
            sub_01.len(arena).expect("should exist"),
            0,
            "new sub should have len 0"
        );
        assert_eq!(
            sub_01.is_empty(arena).expect("should exist"),
            true,
            "new sub should be empty"
        );

        let mut sub_02 = arena.alloc();
        assert_eq!(
            sub_02.len(arena).expect("should exist"),
            0,
            "new sub should have len 0"
        );
        assert_eq!(
            sub_02.is_empty(arena).expect("should exist"),
            true,
            "new sub should be empty"
        );

        test_routine(arena, &mut sub_01);
        test_routine(arena, &mut sub_02);
    }

    #[test]
    fn one_then_other() {
        let arena = &mut CacheArena::default();

        assert_eq!(arena.backing.len(), 0, "should start with an empty backing");
        assert_eq!(arena.offsets.len(), 0, "should start without any offsets");

        let mut sub_01 = arena.alloc();
        assert_eq!(
            sub_01.len(arena).expect("should exist"),
            0,
            "new sub should have len 0"
        );
        assert_eq!(
            sub_01.is_empty(arena).expect("should exist"),
            true,
            "new sub should be empty"
        );

        test_routine(arena, &mut sub_01);

        let mut sub_02 = arena.alloc();
        assert_eq!(
            sub_02.len(arena).expect("should exist"),
            0,
            "new sub should have len 0"
        );
        assert_eq!(
            sub_02.is_empty(arena).expect("should exist"),
            true,
            "new sub should be empty"
        );

        test_routine(arena, &mut sub_02);
        test_routine(arena, &mut sub_01);
        test_routine(arena, &mut sub_02);
    }

    #[test]
    fn many() {
        let arena = &mut CacheArena::default();

        assert_eq!(arena.backing.len(), 0, "should start with an empty backing");
        assert_eq!(arena.offsets.len(), 0, "should start without any offsets");

        let mut subs = vec![];

        for i in 0..50 {
            if i == 0 {
                let sub = arena.alloc();
                assert_eq!(
                    sub.len(arena).expect("should exist"),
                    0,
                    "new sub should have len 0"
                );
                assert_eq!(
                    sub.is_empty(arena).expect("should exist"),
                    true,
                    "new sub should be empty"
                );
                subs.push(sub);

                continue;
            } else if i % 2 == 0 {
                test_routine(arena, &mut subs[i - 1]);
            }

            let sub = arena.alloc();
            assert_eq!(
                sub.len(arena).expect("should exist"),
                0,
                "new sub should have len 0"
            );
            assert_eq!(
                sub.is_empty(arena).expect("should exist"),
                true,
                "new sub should be empty"
            );
            subs.push(sub);
        }

        for mut sub in subs.iter_mut() {
            test_routine(arena, &mut sub);
        }
    }
}
