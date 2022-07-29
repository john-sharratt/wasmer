use crate::js::store::AsStoreRef;
use crate::js::MemoryAccessError;
use std::convert::TryInto;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::slice;
use tracing::warn;

use wasmer_types::{Bytes, Pages};

use super::Memory;
use super::memory::MemoryBuffer;

/// A WebAssembly `memory` view.
///
/// A memory view is used to read and write to the linear memory.
///
/// After a memory is grown a view must not be used anymore. Views are
/// created using the Memory.grow() method.
#[derive(Debug)]
pub struct MemoryView<'a> {
    view: js_sys::Uint8Array,
    size: u64,
    marker: PhantomData<&'a Memory>,
}

impl<'a> MemoryView<'a>
{
    pub(crate) fn new(memory: &Memory, store: &impl AsStoreRef) -> Self {
        let buffer = memory
            .handle
            .get(store.as_store_ref().objects())
            .memory
            .buffer();

        let size = js_sys::Reflect::get(
                &buffer,
                &"byteLength".into(),
            )
            .unwrap()
            .as_f64()
            .unwrap() as u64;

        let view = js_sys::Uint8Array::new(
            &buffer,
        );

        Self {
            view,
            size,
            marker: PhantomData,
        }
    }

    /// Returns the pointer to the raw bytes of the `Memory`.
    #[doc(hidden)]
    pub fn data_ptr(&self) -> *mut u8 {
        unimplemented!("direct data pointer access is not possible in JavaScript");
    }

    /// Returns the size (in bytes) of the `Memory`.
    pub fn data_size(&self) -> u64 {
        self.size
    }

    /// Returns the size (in [`Pages`]) of the `Memory`.
    ///
    /// # Example
    ///
    /// ```
    /// # use wasmer::{Memory, MemoryType, Pages, Store, Type, Value};
    /// # let mut store = Store::default();
    /// #
    /// let m = Memory::new(&store, MemoryType::new(1, None, false)).unwrap();
    ///
    /// assert_eq!(m.size(), Pages(1));
    /// ```
    pub fn size(&self) -> Pages {
        Bytes(self.size as usize).try_into().unwrap()
    }

    /// Used by tests
    #[doc(hidden)]
    pub fn uint8view(&self) -> js_sys::Uint8Array {
        self.view.clone()
    }

    pub(crate) fn buffer(&self) -> MemoryBuffer<'a> {
        MemoryBuffer {
            base: &self.view as *const _ as *mut _,
            marker: PhantomData,
        }
    }

    /// Safely reads bytes from the memory at the given offset.
    ///
    /// The full buffer will be filled, otherwise a `MemoryAccessError` is returned
    /// to indicate an out-of-bounds access.
    ///
    /// This method is guaranteed to be safe (from the host side) in the face of
    /// concurrent writes.
    pub fn read(
        &self,
        offset: u64,
        data: &mut [u8],
    ) -> Result<(), MemoryAccessError> {
        let view = &self.view;
        let offset: u32 = offset.try_into().map_err(|_| MemoryAccessError::Overflow)?;
        let len: u32 = data
            .len()
            .try_into()
            .map_err(|_| MemoryAccessError::Overflow)?;
        let end = offset.checked_add(len).ok_or(MemoryAccessError::Overflow)?;
        if end > view.length() {
            warn!("attempted to read ({} bytes) beyond the bounds of the memory view ({} > {})", len, end, view.length());
            Err(MemoryAccessError::HeapOutOfBounds)?;
        }
        view.subarray(offset, end).copy_to(data);
        Ok(())
    }

    /// Safely reads bytes from the memory at the given offset.
    ///
    /// This method is similar to `read` but allows reading into an
    /// uninitialized buffer. An initialized view of the buffer is returned.
    ///
    /// The full buffer will be filled, otherwise a `MemoryAccessError` is returned
    /// to indicate an out-of-bounds access.
    ///
    /// This method is guaranteed to be safe (from the host side) in the face of
    /// concurrent writes.
    pub fn read_uninit(
        &self,
        offset: u64,
        buf: &'a mut [MaybeUninit<u8>],
    ) -> Result<&'a mut [u8], MemoryAccessError> {
        let view = &self.view;
        let offset: u32 = offset.try_into().map_err(|_| MemoryAccessError::Overflow)?;
        let len: u32 = buf
            .len()
            .try_into()
            .map_err(|_| MemoryAccessError::Overflow)?;
        let end = offset.checked_add(len).ok_or(MemoryAccessError::Overflow)?;
        if end > view.length() {
            warn!("attempted to read ({} bytes) beyond the bounds of the memory view ({} > {})", len, end, view.length());
            Err(MemoryAccessError::HeapOutOfBounds)?;
        }

        // Zero-initialize the buffer to avoid undefined behavior with
        // uninitialized data.
        for elem in buf.iter_mut() {
            *elem = MaybeUninit::new(0);
        }
        let buf = unsafe { slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len()) };

        view.subarray(offset, end).copy_to(buf);
        Ok(buf)
    }

    /// Safely writes bytes to the memory at the given offset.
    ///
    /// If the write exceeds the bounds of the memory then a `MemoryAccessError` is
    /// returned.
    ///
    /// This method is guaranteed to be safe (from the host side) in the face of
    /// concurrent reads/writes.
    pub fn write(
        &self,
        offset: u64,
        data: &[u8],
    ) -> Result<(), MemoryAccessError> {
        let offset: u32 = offset.try_into().map_err(|_| MemoryAccessError::Overflow)?;
        let len: u32 = data
            .len()
            .try_into()
            .map_err(|_| MemoryAccessError::Overflow)?;
        let view = &self.view;
        let end = offset.checked_add(len).ok_or(MemoryAccessError::Overflow)?;
        if end > view.length() {
            warn!("attempted to write ({} bytes) beyond the bounds of the memory view ({} > {})", len, end, view.length());
            Err(MemoryAccessError::HeapOutOfBounds)?;
        }
        view.subarray(offset, end).copy_from(data);
        Ok(())
    }
}
