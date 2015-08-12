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

struct AllocInfo {
    refcount: AtomicUsize,
    buf: [u8]
}

/// A read-only view into an AppendBuf.
pub struct Slice {
    alloc: *mut AllocInfo,
    offset: usize,
    len: usize
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

    /// Get the remaining space in the AppendBuf for writing.
    ///
    /// If you wish the see the data written in subsequent Slices,
    /// you must also call `advance` with the amount written.
    ///
    /// Reads from this buffer are reads into uninitalized memory,
    /// and so should be carefully avoided.
    pub fn get_write_buf(&mut self) -> &mut [u8] {
         &mut self.allocinfo_mut().buf
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

impl io::Write for AppendBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let amount = try!(self.get_write_buf().write(buf));
        self.position += amount;

        Ok(amount)
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

impl Deref for Slice {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        unsafe { &(*self.alloc).buf[self.offset..self.len] }
    }
}

impl AsRef<[u8]> for Slice {
    fn as_ref(&self) -> &[u8] { self }
}

impl AllocInfo {
    unsafe fn allocate(size: usize) -> *mut Self {
        let alloc = memalloc::allocate(size + std::mem::size_of::<AtomicUsize>());
        let this = mem::transmute::<_, *mut Self>((alloc, size));
        (*this).refcount = AtomicUsize::new(0);
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

#[test]
fn test_write_and_slice() {
    use std::io::Write;

    let mut buf = AppendBuf::new(10);
    assert_eq!(buf.write(&[1, 2, 3]).unwrap(), 3);
    let slice = buf.slice();
    assert_eq!(&*slice, &[1, 2, 3]);
}

#[test]
fn test_overlong_write() {
    use std::io::Write;

    let mut buf = AppendBuf::new(5);
    assert_eq!(buf.write(&[1, 2, 3, 4, 5, 6]).unwrap(), 5);
    let slice = buf.slice();
    assert_eq!(&*slice, &[1, 2, 3, 4, 5]);
}

