//
// Copyright 2019 The Project Oak Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

// See https://github.com/rust-lang/rust/blob/6b561b4917e803c4be4ca44d8e552b680cb9e380/src/libstd/sys/unix/mutex.rs

use crate::core::cell::UnsafeCell;
use crate::core::mem::MaybeUninit;

pub struct Mutex { inner: UnsafeCell<libc::pthread_mutex_t> }

#[inline]
pub unsafe fn raw(m: &Mutex) -> *mut libc::pthread_mutex_t {
    m.inner.get()
}

unsafe impl Send for Mutex {}
unsafe impl Sync for Mutex {}

impl Mutex {
    pub const fn new() -> Mutex {
        // Might be moved to a different address, so it is better to avoid
        // initialization of potentially opaque OS data before it landed.
        // Be very careful using this newly constructed `Mutex`, reentrant
        // locking is undefined behavior until `init` is called!
        Mutex { inner: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER) }
    }
    #[inline]
    pub unsafe fn init(&mut self) {
        // Issue #33770
        //
        // A pthread mutex initialized with PTHREAD_MUTEX_INITIALIZER will have
        // a type of PTHREAD_MUTEX_DEFAULT, which has undefined behavior if you
        // try to re-lock it from the same thread when you already hold a lock.
        //
        // In practice, glibc takes advantage of this undefined behavior to
        // implement hardware lock elision, which uses hardware transactional
        // memory to avoid acquiring the lock. While a transaction is in
        // progress, the lock appears to be unlocked. This isn't a problem for
        // other threads since the transactional memory will abort if a conflict
        // is detected, however no abort is generated if re-locking from the
        // same thread.
        //
        // Since locking the same mutex twice will result in two aliasing &mut
        // references, we instead create the mutex with type
        // PTHREAD_MUTEX_NORMAL which is guaranteed to deadlock if we try to
        // re-lock it from the same thread, thus avoiding undefined behavior.
        let mut attr = MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
        let r = libc::pthread_mutexattr_init(attr.as_mut_ptr());
        debug_assert_eq!(r, 0);
        let r = libc::pthread_mutexattr_settype(attr.as_mut_ptr(), libc::PTHREAD_MUTEX_NORMAL);
        debug_assert_eq!(r, 0);
        let r = libc::pthread_mutex_init(self.inner.get(), attr.as_ptr());
        debug_assert_eq!(r, 0);
        let r = libc::pthread_mutexattr_destroy(attr.as_mut_ptr());
        debug_assert_eq!(r, 0);
    }
    #[inline]
    pub unsafe fn lock(&self) {
        let r = libc::pthread_mutex_lock(self.inner.get());
        debug_assert_eq!(r, 0);
    }
    #[inline]
    pub unsafe fn unlock(&self) {
        let r = libc::pthread_mutex_unlock(self.inner.get());
        debug_assert_eq!(r, 0);
    }
    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        libc::pthread_mutex_trylock(self.inner.get()) == 0
    }
    #[inline]
    pub unsafe fn destroy(&self) {
        let r = libc::pthread_mutex_destroy(self.inner.get());
        debug_assert_eq!(r, 0);
    }
}

pub struct ReentrantMutex { inner: UnsafeCell<libc::pthread_mutex_t> }

unsafe impl Send for ReentrantMutex {}
unsafe impl Sync for ReentrantMutex {}

impl ReentrantMutex {
    pub unsafe fn uninitialized() -> ReentrantMutex {
        ReentrantMutex { inner: UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER) }
    }

    pub unsafe fn init(&mut self) {
        let mut attr = MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
        let result = libc::pthread_mutexattr_init(attr.as_mut_ptr());
        debug_assert_eq!(result, 0);
        let result = libc::pthread_mutexattr_settype(attr.as_mut_ptr(),
                                                    libc::PTHREAD_MUTEX_RECURSIVE);
        debug_assert_eq!(result, 0);
        let result = libc::pthread_mutex_init(self.inner.get(), attr.as_ptr());
        debug_assert_eq!(result, 0);
        let result = libc::pthread_mutexattr_destroy(attr.as_mut_ptr());
        debug_assert_eq!(result, 0);
    }

    pub unsafe fn lock(&self) {
        let result = libc::pthread_mutex_lock(self.inner.get());
        debug_assert_eq!(result, 0);
    }

    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        libc::pthread_mutex_trylock(self.inner.get()) == 0
    }

    pub unsafe fn unlock(&self) {
        let result = libc::pthread_mutex_unlock(self.inner.get());
        debug_assert_eq!(result, 0);
    }

    pub unsafe fn destroy(&self) {
        let result = libc::pthread_mutex_destroy(self.inner.get());
        debug_assert_eq!(result, 0);
    }
}