//! Single producer single consumer ring buffer.
//!
//! Used by the capture thread to hand samples to the DSP thread without a
//! lock. Capacity is rounded up to a power of two so the index wrap is a mask.
//! On overflow the writer drops the incoming block and increments a counter;
//! for a receiver this is the correct behaviour, the UI reports the overrun
//! instead of stalling the audio callback.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct RingBuffer<T: Copy> {
    data: UnsafeCell<Box<[T]>>,
    mask: usize,
    write: AtomicUsize,
    read: AtomicUsize,
    overruns: AtomicUsize,
}

// Safety: exactly one Producer and one Consumer exist per buffer, each owning
// its own index. Data slots touched by the two sides never overlap because
// they are bounded by the opposite index loaded with Acquire ordering.
unsafe impl<T: Copy + Send> Send for RingBuffer<T> {}
unsafe impl<T: Copy + Send> Sync for RingBuffer<T> {}

pub struct Producer<T: Copy> {
    inner: Arc<RingBuffer<T>>,
}

pub struct Consumer<T: Copy> {
    inner: Arc<RingBuffer<T>>,
}

pub fn channel<T: Copy + Default>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let cap = capacity.max(2).next_power_of_two();
    let buf = vec![T::default(); cap].into_boxed_slice();
    let inner = Arc::new(RingBuffer {
        data: UnsafeCell::new(buf),
        mask: cap - 1,
        write: AtomicUsize::new(0),
        read: AtomicUsize::new(0),
        overruns: AtomicUsize::new(0),
    });
    (Producer { inner: inner.clone() }, Consumer { inner })
}

impl<T: Copy> RingBuffer<T> {
    #[inline]
    fn capacity(&self) -> usize {
        self.mask + 1
    }

    #[inline]
    fn ptr(&self) -> *mut T {
        unsafe { (*self.data.get()).as_mut_ptr() }
    }
}

impl<T: Copy> Producer<T> {
    /// Writes the whole slice or nothing. Returns false when the buffer is
    /// full; the overrun counter is bumped in that case.
    pub fn write(&self, src: &[T]) -> bool {
        let r = self.inner.read.load(Ordering::Acquire);
        let w = self.inner.write.load(Ordering::Relaxed);
        let free = self.inner.capacity() - (w.wrapping_sub(r));
        if src.len() > free {
            self.inner.overruns.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        let base = self.inner.ptr();
        let start = w & self.inner.mask;
        let first = (self.inner.capacity() - start).min(src.len());
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(start), first);
            if first < src.len() {
                std::ptr::copy_nonoverlapping(src.as_ptr().add(first), base, src.len() - first);
            }
        }
        self.inner.write.store(w.wrapping_add(src.len()), Ordering::Release);
        true
    }

    pub fn overruns(&self) -> usize {
        self.inner.overruns.load(Ordering::Relaxed)
    }
}

impl<T: Copy> Consumer<T> {
    /// Number of samples ready to be read.
    pub fn len(&self) -> usize {
        let w = self.inner.write.load(Ordering::Acquire);
        let r = self.inner.read.load(Ordering::Relaxed);
        w.wrapping_sub(r)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Copies up to dst.len() samples and advances the read index.
    pub fn read(&self, dst: &mut [T]) -> usize {
        let avail = self.len().min(dst.len());
        if avail == 0 {
            return 0;
        }
        let r = self.inner.read.load(Ordering::Relaxed);
        let base = self.inner.ptr();
        let start = r & self.inner.mask;
        let first = (self.inner.capacity() - start).min(avail);
        unsafe {
            std::ptr::copy_nonoverlapping(base.add(start), dst.as_mut_ptr(), first);
            if first < avail {
                std::ptr::copy_nonoverlapping(base, dst.as_mut_ptr().add(first), avail - first);
            }
        }
        self.inner.read.store(r.wrapping_add(avail), Ordering::Release);
        avail
    }

    /// Drops samples without copying. Used to resync after a long UI stall.
    pub fn skip(&self, count: usize) -> usize {
        let n = self.len().min(count);
        let r = self.inner.read.load(Ordering::Relaxed);
        self.inner.read.store(r.wrapping_add(n), Ordering::Release);
        n
    }

    pub fn overruns(&self) -> usize {
        self.inner.overruns.load(Ordering::Relaxed)
    }
}