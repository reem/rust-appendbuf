#![cfg_attr(test, deny(warnings))]
#![deny(missing_docs)]

//! # appendbuf
//!
//! A Sync append-only buffer with Send views.
//!

extern crate memalloc;

use std::sync::atomic::{self, AtomicUsize, Ordering};
use std::ops::Deref;
use std::{io, mem};

/// An append-only, atomically reference counted buffer.
pub struct AppendBuf {
    alloc: *mut AllocInfo,
    position: usize
}

unsafe impl Send for AppendBuf {}
unsafe impl Sync for AppendBuf {}

struct AllocInfo {
    refcount: AtomicUsize,
    buf: [u8]
}

unsafe impl Send for AllocInfo {}
unsafe impl Sync for AllocInfo {}

/// A read-only view into an AppendBuf.
pub struct Slice {
    alloc: *mut AllocInfo,
    offset: usize,
    len: usize
}

unsafe impl Send for Slice {}
unsafe impl Sync for Slice {}

impl Slice {
    /// Get a subslice starting from the passed offset.
    pub fn slice_from(&self, offset: usize) -> Slice {
        if self.len < self.offset + offset {
            panic!("Sliced past the end of an appendbuf::Slice,
                   the length was {:?} and the desired offset was {:?}",
                   self.len, offset);
        }

        self.allocinfo().increment();

        Slice {
            alloc: self.alloc,
            offset: self.offset + offset,
            len: self.len - offset
        }
    }

    /// Get a subslice of the first len elements.
    pub fn slice_to(&self, len: usize) -> Slice {
        if self.len < len {
            panic!("Sliced past the end of an appendbuf::Slice,
                   the length was {:?} and the desired length was {:?}",
                   self.len, len);
        }

        self.allocinfo().increment();

        Slice {
            alloc: self.alloc,
            offset: self.offset,
            len: len
        }
    }

    /// Get a subslice starting at the passed `start` offset and ending at
    /// the passed `end` offset.
    pub fn slice(&self, start: usize, end: usize) -> Slice {
        let slice = self.slice_from(start);
        slice.slice_to(end - start)
    }

    fn allocinfo(&self) -> &AllocInfo {
        unsafe { mem::transmute(self.alloc) }
    }
}

impl AppendBuf {
    /// Create a new, empty AppendBuf with the given capacity.
    pub fn new(len: usize) -> AppendBuf {
        AppendBuf {
            alloc: unsafe { AllocInfo::allocate(len) },
            position: 0
        }
    }

    /// Create a new Slice of the entire AppendBuf so far.
    pub fn slice(&self) -> Slice {
        self.allocinfo().increment();

        Slice {
            alloc: self.alloc,
            offset: 0,
            len: self.position
        }
    }

    /// Retrieve the amount of remaining space in the AppendBuf.
    pub fn remaining(&self) -> usize {
        self.allocinfo().buf.len() - self.position
    }

    /// Write the data in the passed buffer onto the AppendBuf.
    ///
    /// This is an alternative to using the implementation of `std::io::Write`
    /// which does not unnecessarily use `Result`.
    pub fn fill(&mut self, buf: &[u8]) -> usize {
        use std::io::Write;

        // FIXME: Use std::slice::bytes::copy_memory when it is stabilized.
        let amount = self.get_write_buf().write(buf).unwrap();
        self.position += amount;

        amount
    }

    /// Get the remaining space in the AppendBuf for writing.
    ///
    /// If you wish the see the data written in subsequent Slices,
    /// you must also call `advance` with the amount written.
    ///
    /// Reads from this buffer are reads into uninitalized memory,
    /// and so should be carefully avoided.
    pub fn get_write_buf(&mut self) -> &mut [u8] {
        let position = self.position;
         &mut self.allocinfo_mut().buf[position..]
    }

    /// Advance the position of the AppendBuf.
    ///
    /// You should only advance the buffer if you have written to a
    /// buffer returned by `get_write_buf`.
    pub unsafe fn advance(&mut self, amount: usize) {
         self.position += amount;
    }

    fn allocinfo(&self) -> &AllocInfo {
        unsafe { mem::transmute(self.alloc) }
    }

    fn allocinfo_mut(&mut self) -> &mut AllocInfo {
        unsafe { mem::transmute(self.alloc) }
    }
}

impl Deref for AppendBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.allocinfo().buf[..self.position]
    }
}

impl AsRef<[u8]> for AppendBuf {
    fn as_ref(&self) -> &[u8] { self }
}

impl io::Write for AppendBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(self.fill(buf))
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

impl Deref for Slice {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { &(*self.alloc).buf[self.offset..self.offset + self.len] }
    }
}

impl AsRef<[u8]> for Slice {
    fn as_ref(&self) -> &[u8] { self }
}

impl AllocInfo {
    unsafe fn allocate(size: usize) -> *mut Self {
        let alloc = memalloc::allocate(size + std::mem::size_of::<AtomicUsize>());
        let this = mem::transmute::<_, *mut Self>((alloc, size));
        (*this).refcount = AtomicUsize::new(1);
        this
    }

    #[inline(always)]
    fn increment(&self) {
         self.refcount.fetch_add(1, Ordering::Relaxed);
    }

    #[inline(always)]
    unsafe fn decrement(&self) {
        // Adapted from the implementation of Drop for std::sync::Arc.

        // Because `fetch_sub` is already atomic, we do not need to synchronize
        // with other threads unless we are going to deallocate the buffer.
        if self.refcount.fetch_sub(1, Ordering::Release) != 1 { return }

        // This fence is needed to prevent reordering of use of the data and
        // deletion of the data. Because it is marked `Release`, the decreasing
        // of the reference count synchronizes with this `Acquire` fence. This
        // means that use of the data happens before decreasing the reference
        // count, which happens before this fence, which happens before the
        // deletion of the data.
        //
        // As explained in the [Boost documentation][1],
        //
        // > It is important to enforce any possible access to the object in one
        // > thread (through an existing reference) to *happen before* deleting
        // > the object in a different thread. This is achieved by a "release"
        // > operation after dropping a reference (any access to the object
        // > through this reference must obviously happened before), and an
        // > "acquire" operation before deleting the object.
        //
        // [1]: (www.boost.org/doc/libs/1_55_0/doc/html/atomic/usage_examples.html)
        atomic::fence(Ordering::Acquire);

        drop(mem::transmute::<&AllocInfo, Box<AllocInfo>>(self))
    }
}

impl Drop for Slice {
    fn drop(&mut self) {
        unsafe { (*self.alloc).decrement() }
    }
}

impl Drop for AppendBuf {
    fn drop(&mut self) {
        unsafe { (*self.alloc).decrement() }
    }
}

fn _compile_test() {
    fn _is_send_sync<T: Send + Sync>() {}
    _is_send_sync::<AppendBuf>();
    _is_send_sync::<Slice>();
}

#[test]
fn test_write_and_slice() {
    let mut buf = AppendBuf::new(10);
    assert_eq!(buf.fill(&[1, 2, 3]), 3);
    let slice = buf.slice();
    assert_eq!(&*slice, &[1, 2, 3]);

    assert_eq!(&*buf, &[1, 2, 3]);
}

#[test]
fn test_overlong_write() {
    let mut buf = AppendBuf::new(5);
    assert_eq!(buf.fill(&[1, 2, 3, 4, 5, 6]), 5);
    let slice = buf.slice();
    assert_eq!(&*slice, &[1, 2, 3, 4, 5]);
}

#[test]
fn test_slice_slicing() {
    let data = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    let mut buf = AppendBuf::new(10);
    assert_eq!(buf.fill(data), 10);

    assert_eq!(&*buf.slice(), data);
    assert_eq!(&*buf.slice().slice_to(5), &data[..5]);
    assert_eq!(&*buf.slice().slice_from(6), &data[6..]);
    assert_eq!(&*buf.slice().slice(2, 7), &data[2..7]);
}

#[test]
fn test_many_writes() {
    let mut buf = AppendBuf::new(100);

    assert_eq!(buf.fill(&[1, 2, 3, 4]), 4);
    assert_eq!(buf.fill(&[10, 12, 13, 14, 15]), 5);
    assert_eq!(buf.fill(&[34, 35]), 2);

    assert_eq!(&*buf.slice(), &[1, 2, 3, 4, 10, 12, 13, 14, 15, 34, 35]);
}

#[test]
fn test_slice_then_write() {
    let mut buf = AppendBuf::new(20);
    let empty = buf.slice();
    assert_eq!(&*empty, &[]);

    assert_eq!(buf.fill(&[5, 6, 7, 8]), 4);

    let not_empty = buf.slice();
    assert_eq!(&*empty, &[]);
    assert_eq!(&*not_empty, &[5, 6, 7, 8]);

    assert_eq!(buf.fill(&[9, 10, 11, 12, 13]), 5);
    assert_eq!(&*empty, &[]);
    assert_eq!(&*not_empty, &[5, 6, 7, 8]);
    assert_eq!(&*buf.slice(), &[5, 6, 7, 8, 9, 10, 11, 12, 13]);
}

#[test]
fn test_slice_bounds_edge_cases() {
    let data = &[1, 2, 3, 4, 5, 6];

    let mut buf = AppendBuf::new(data.len());
    assert_eq!(buf.fill(data), data.len());

    let slice = buf.slice().slice_to(data.len());
    assert_eq!(&*slice, data);

    let slice = buf.slice().slice_from(0);
    assert_eq!(&*slice, data);
}

#[test]
#[should_panic = "the desired offset"]
fn test_slice_from_bounds_checks() {
    let data = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    let mut buf = AppendBuf::new(10);
    assert_eq!(buf.fill(data), 10);

    buf.slice().slice_from(100);
}

#[test]
#[should_panic = "the desired length"]
fn test_slice_to_bounds_checks() {
    let data = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    let mut buf = AppendBuf::new(10);
    assert_eq!(buf.fill(data), 10);

    buf.slice().slice_to(100);
}

