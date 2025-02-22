// This file contains code from external sources.
// Attributions: https://github.com/wasmerio/wasmer/blob/master/ATTRIBUTIONS.md

//! Memory management for linear memories.
//!
//! `Memory` is to WebAssembly linear memories what `Table` is to WebAssembly tables.

use crate::vmcontext::VMMemoryDefinition;
use crate::{mmap::Mmap, store::MaybeInstanceOwned};
use more_asserts::assert_ge;
use std::cell::UnsafeCell;
use std::convert::TryInto;
use std::ptr::NonNull;
use thiserror::Error;
use wasmer_types::{Bytes, MemoryStyle, MemoryType, Pages};

/// Error type describing things that can go wrong when operating on Wasm Memories.
#[derive(Error, Debug, Clone, Eq, PartialEq, Hash)]
pub enum MemoryError {
    /// Low level error with mmap.
    #[error("Error when allocating memory: {0}")]
    Region(String),
    /// The operation would cause the size of the memory to exceed the maximum or would cause
    /// an overflow leading to unindexable memory.
    #[error("The memory could not grow: current size {} pages, requested increase: {} pages", current.0, attempted_delta.0)]
    CouldNotGrow {
        /// The current size in pages.
        current: Pages,
        /// The attempted amount to grow by in pages.
        attempted_delta: Pages,
    },
    /// The operation would cause the size of the memory size exceed the maximum.
    #[error("The memory is invalid because {}", reason)]
    InvalidMemory {
        /// The reason why the provided memory is invalid.
        reason: String,
    },
    /// Caller asked for more minimum memory than we can give them.
    #[error("The minimum requested ({} pages) memory is greater than the maximum allowed memory ({} pages)", min_requested.0, max_allowed.0)]
    MinimumMemoryTooLarge {
        /// The number of pages requested as the minimum amount of memory.
        min_requested: Pages,
        /// The maximum amount of memory we can allocate.
        max_allowed: Pages,
    },
    /// Caller asked for a maximum memory greater than we can give them.
    #[error("The maximum requested memory ({} pages) is greater than the maximum allowed memory ({} pages)", max_requested.0, max_allowed.0)]
    MaximumMemoryTooLarge {
        /// The number of pages requested as the maximum amount of memory.
        max_requested: Pages,
        /// The number of pages requested as the maximum amount of memory.
        max_allowed: Pages,
    },
    /// A user defined error value, used for error cases not listed above.
    #[error("A user-defined error occurred: {0}")]
    Generic(String),
}

/// A linear memory instance.
pub struct VMMemory {
    // The underlying allocation.
    mmap: WasmMmap,

    // The optional maximum size in wasm pages of this linear memory.
    maximum: Option<Pages>,

    /// The WebAssembly linear memory description.
    memory: MemoryType,

    /// Our chosen implementation style.
    style: MemoryStyle,

    // Size in bytes of extra guard pages after the end to optimize loads and stores with
    // constant offsets.
    offset_guard_size: usize,

    /// The owned memory definition used by the generated code
    vm_memory_definition: MaybeInstanceOwned<VMMemoryDefinition>,
}

#[derive(Debug)]
struct WasmMmap {
    // Our OS allocation of mmap'd memory.
    alloc: Mmap,
    // The current logical size in wasm pages of this linear memory.
    size: Pages,
}

impl VMMemory {
    /// Create a new linear memory instance with specified minimum and maximum number of wasm pages.
    ///
    /// This creates a `Memory` with owned metadata: this can be used to create a memory
    /// that will be imported into Wasm modules.
    pub fn new(memory: &MemoryType, style: &MemoryStyle) -> Result<Self, MemoryError> {
        unsafe { Self::new_internal(memory, style, None) }
    }

    /// Create a new linear memory instance with specified minimum and maximum number of wasm pages.
    ///
    /// This creates a `Memory` with metadata owned by a VM, pointed to by
    /// `vm_memory_location`: this can be used to create a local memory.
    ///
    /// # Safety
    /// - `vm_memory_location` must point to a valid location in VM memory.
    pub unsafe fn from_definition(
        memory: &MemoryType,
        style: &MemoryStyle,
        vm_memory_location: NonNull<VMMemoryDefinition>,
    ) -> Result<Self, MemoryError> {
        Self::new_internal(memory, style, Some(vm_memory_location))
    }

    /// Build a `Memory` with either self-owned or VM owned metadata.
    unsafe fn new_internal(
        memory: &MemoryType,
        style: &MemoryStyle,
        vm_memory_location: Option<NonNull<VMMemoryDefinition>>,
    ) -> Result<Self, MemoryError> {
        if memory.minimum > Pages::max_value() {
            return Err(MemoryError::MinimumMemoryTooLarge {
                min_requested: memory.minimum,
                max_allowed: Pages::max_value(),
            });
        }
        // `maximum` cannot be set to more than `65536` pages.
        if let Some(max) = memory.maximum {
            if max > Pages::max_value() {
                return Err(MemoryError::MaximumMemoryTooLarge {
                    max_requested: max,
                    max_allowed: Pages::max_value(),
                });
            }
            if max < memory.minimum {
                return Err(MemoryError::InvalidMemory {
                    reason: format!(
                        "the maximum ({} pages) is less than the minimum ({} pages)",
                        max.0, memory.minimum.0
                    ),
                });
            }
        }

        let offset_guard_bytes = style.offset_guard_size() as usize;

        let minimum_pages = match style {
            MemoryStyle::Dynamic { .. } => memory.minimum,
            MemoryStyle::Static { bound, .. } => {
                assert_ge!(*bound, memory.minimum);
                *bound
            }
        };
        let minimum_bytes = minimum_pages.bytes().0;
        let request_bytes = minimum_bytes.checked_add(offset_guard_bytes).unwrap();
        let mapped_pages = memory.minimum;
        let mapped_bytes = mapped_pages.bytes();

        let mut mmap = WasmMmap {
            alloc: Mmap::accessible_reserved(mapped_bytes.0, request_bytes)
                .map_err(MemoryError::Region)?,
            size: memory.minimum,
        };

        let base_ptr = mmap.alloc.as_mut_ptr();
        let mem_length = memory.minimum.bytes().0;
        Ok(Self {
            mmap,
            maximum: memory.maximum,
            offset_guard_size: offset_guard_bytes,
            vm_memory_definition: if let Some(mem_loc) = vm_memory_location {
                {
                    let mut ptr = mem_loc;
                    let md = ptr.as_mut();
                    md.base = base_ptr;
                    md.current_length = mem_length;
                }
                MaybeInstanceOwned::Instance(mem_loc)
            } else {
                MaybeInstanceOwned::Host(Box::new(UnsafeCell::new(VMMemoryDefinition {
                    base: base_ptr,
                    current_length: mem_length,
                })))
            },
            memory: *memory,
            style: style.clone(),
        })
    }

    /// Get the `VMMemoryDefinition`.
    fn get_vm_memory_definition(&self) -> NonNull<VMMemoryDefinition> {
        self.vm_memory_definition.as_ptr()
    }

    /// Returns the type for this memory.
    pub fn ty(&self) -> MemoryType {
        let minimum = self.size();
        let mut out = self.memory;
        out.minimum = minimum;

        out
    }

    /// Returns the memory style for this memory.
    pub fn style(&self) -> &MemoryStyle {
        &self.style
    }

    /// Returns the number of allocated wasm pages.
    pub fn size(&self) -> Pages {
        // TODO: investigate this function for race conditions
        unsafe {
            let md_ptr = self.get_vm_memory_definition();
            let md = md_ptr.as_ref();
            Bytes::from(md.current_length).try_into().unwrap()
        }
    }

    /// Grow memory by the specified amount of wasm pages.
    ///
    /// Returns `None` if memory can't be grown by the specified amount
    /// of wasm pages.
    pub fn grow(&mut self, delta: Pages) -> Result<Pages, MemoryError> {
        // Optimization of memory.grow 0 calls.
        if delta.0 == 0 {
            return Ok(self.mmap.size);
        }

        let new_pages = self
            .mmap
            .size
            .checked_add(delta)
            .ok_or(MemoryError::CouldNotGrow {
                current: self.mmap.size,
                attempted_delta: delta,
            })?;
        let prev_pages = self.mmap.size;

        if let Some(maximum) = self.maximum {
            if new_pages > maximum {
                return Err(MemoryError::CouldNotGrow {
                    current: self.mmap.size,
                    attempted_delta: delta,
                });
            }
        }

        // Wasm linear memories are never allowed to grow beyond what is
        // indexable. If the memory has no maximum, enforce the greatest
        // limit here.
        if new_pages >= Pages::max_value() {
            // Linear memory size would exceed the index range.
            return Err(MemoryError::CouldNotGrow {
                current: self.mmap.size,
                attempted_delta: delta,
            });
        }

        let delta_bytes = delta.bytes().0;
        let prev_bytes = prev_pages.bytes().0;
        let new_bytes = new_pages.bytes().0;

        if new_bytes > self.mmap.alloc.len() - self.offset_guard_size {
            // If the new size is within the declared maximum, but needs more memory than we
            // have on hand, it's a dynamic heap and it can move.
            let guard_bytes = self.offset_guard_size;
            let request_bytes =
                new_bytes
                    .checked_add(guard_bytes)
                    .ok_or_else(|| MemoryError::CouldNotGrow {
                        current: new_pages,
                        attempted_delta: Bytes(guard_bytes).try_into().unwrap(),
                    })?;

            let mut new_mmap =
                Mmap::accessible_reserved(new_bytes, request_bytes).map_err(MemoryError::Region)?;

            let copy_len = self.mmap.alloc.len() - self.offset_guard_size;
            new_mmap.as_mut_slice()[..copy_len]
                .copy_from_slice(&self.mmap.alloc.as_slice()[..copy_len]);

            self.mmap.alloc = new_mmap;
        } else if delta_bytes > 0 {
            // Make the newly allocated pages accessible.
            self.mmap
                .alloc
                .make_accessible(prev_bytes, delta_bytes)
                .map_err(MemoryError::Region)?;
        }

        self.mmap.size = new_pages;

        // update memory definition
        unsafe {
            let mut md_ptr = self.get_vm_memory_definition();
            let md = md_ptr.as_mut();
            md.current_length = new_pages.bytes().0;
            md.base = self.mmap.alloc.as_mut_ptr() as _;
        }

        Ok(prev_pages)
    }

    /// Return a `VMMemoryDefinition` for exposing the memory to compiled wasm code.
    pub fn vmmemory(&self) -> NonNull<VMMemoryDefinition> {
        self.get_vm_memory_definition()
    }
}
