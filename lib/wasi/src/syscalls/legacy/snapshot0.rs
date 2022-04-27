use crate::syscalls;
use crate::syscalls::types::{self, snapshot0};
use crate::{mem_error_to_wasi, WasiEnv, WasiError, WasiThread};
use wasmer::WasmPtr;

/// Wrapper around `syscalls::fd_filestat_get` with extra logic to handle the size
/// difference of `wasi_filestat_t`
///
/// WARNING: this function involves saving, clobbering, and restoring unrelated
/// Wasm memory.  If the memory clobbered by the current syscall is also used by
/// that syscall, then it may break.
pub fn fd_filestat_get(
    thread: &WasiThread,
    fd: types::__wasi_fd_t,
    buf: WasmPtr<snapshot0::__wasi_filestat_t>,
) -> types::__wasi_errno_t {
    let memory = thread.memory();

    // transmute the WasmPtr<T1> into a WasmPtr<T2> where T2 > T1, this will read extra memory.
    // The edge case of this cenv.mausing an OOB is not handled, if the new field is OOB, then the entire
    // memory access will fail.
    let new_buf: WasmPtr<types::__wasi_filestat_t> = buf.cast();

    // Copy the data including the extra data
    let new_filestat_setup: types::__wasi_filestat_t = wasi_try_mem!(new_buf.read(memory));

    // Set up complete, make the call with the pointer that will write to the
    // struct and some unrelated memory after the struct.
    let result = syscalls::fd_filestat_get(thread, fd, new_buf);

    // reborrow memory
    let memory = thread.memory();

    // get the values written to memory
    let new_filestat = wasi_try_mem!(new_buf.deref(memory).read());
    // translate the new struct into the old struct in host memory
    let old_stat = snapshot0::__wasi_filestat_t {
        st_dev: new_filestat.st_dev,
        st_ino: new_filestat.st_ino,
        st_filetype: new_filestat.st_filetype,
        st_nlink: new_filestat.st_nlink as u32,
        st_size: new_filestat.st_size,
        st_atim: new_filestat.st_atim,
        st_mtim: new_filestat.st_mtim,
        st_ctim: new_filestat.st_ctim,
    };

    // write back the original values at the pointer's memory locations
    // (including the memory unrelated to the pointer)
    wasi_try_mem!(new_buf.deref(memory).write(new_filestat_setup));

    // Now that this memory is back as it was, write the translated filestat
    // into memory leaving it as it should be
    wasi_try_mem!(buf.deref(memory).write(old_stat));

    result
}

/// Wrapper around `syscalls::path_filestat_get` with extra logic to handle the size
/// difference of `wasi_filestat_t`
pub fn path_filestat_get(
    thread: &WasiThread,
    fd: types::__wasi_fd_t,
    flags: types::__wasi_lookupflags_t,
    path: WasmPtr<u8>,
    path_len: u32,
    buf: WasmPtr<snapshot0::__wasi_filestat_t>,
) -> types::__wasi_errno_t {
    // see `fd_filestat_get` in this file for an explanation of this strange behavior
    let memory = thread.memory();

    let new_buf: WasmPtr<types::__wasi_filestat_t> = buf.cast();
    let new_filestat_setup: types::__wasi_filestat_t = wasi_try_mem!(new_buf.read(memory));

    let result = syscalls::path_filestat_get(thread, fd, flags, path, path_len, new_buf);

    let memory = thread.memory();
    let new_filestat = wasi_try_mem!(new_buf.deref(memory).read());
    let old_stat = snapshot0::__wasi_filestat_t {
        st_dev: new_filestat.st_dev,
        st_ino: new_filestat.st_ino,
        st_filetype: new_filestat.st_filetype,
        st_nlink: new_filestat.st_nlink as u32,
        st_size: new_filestat.st_size,
        st_atim: new_filestat.st_atim,
        st_mtim: new_filestat.st_mtim,
        st_ctim: new_filestat.st_ctim,
    };

    wasi_try_mem!(new_buf.deref(memory).write(new_filestat_setup));
    wasi_try_mem!(buf.deref(memory).write(old_stat));

    result
}

/// Wrapper around `syscalls::fd_seek` with extra logic to remap the values
/// of `__wasi_whence_t`
pub fn fd_seek(
    env: &WasiThread,
    fd: types::__wasi_fd_t,
    offset: types::__wasi_filedelta_t,
    whence: snapshot0::__wasi_whence_t,
    newoffset: WasmPtr<types::__wasi_filesize_t>,
) -> Result<types::__wasi_errno_t, WasiError> {
    let new_whence = match whence {
        snapshot0::__WASI_WHENCE_CUR => types::__WASI_WHENCE_CUR,
        snapshot0::__WASI_WHENCE_END => types::__WASI_WHENCE_END,
        snapshot0::__WASI_WHENCE_SET => types::__WASI_WHENCE_SET,
        // if it's invalid, let the new fd_seek handle it
        _ => whence,
    };
    syscalls::fd_seek(env, fd, offset, new_whence, newoffset)
}

/// Wrapper around `syscalls::poll_oneoff` with extra logic to add the removed
/// userdata field back
pub fn poll_oneoff(
    thread: &WasiThread,
    in_: WasmPtr<snapshot0::__wasi_subscription_t>,
    out_: WasmPtr<types::__wasi_event_t>,
    nsubscriptions: u32,
    nevents: WasmPtr<u32>,
) -> Result<types::__wasi_errno_t, WasiError> {
    // in this case the new type is smaller than the old type, so it all fits into memory,
    // we just need to readjust and copy it

    // we start by adjusting `in_` into a format that the new code can understand
    let memory = thread.memory();
    let in_origs = wasi_try_mem_ok!(in_.slice(memory, nsubscriptions));
    let in_origs = wasi_try_mem_ok!(in_origs.read_to_vec());

    // get a pointer to the smaller new type
    let in_new_type_ptr: WasmPtr<types::__wasi_subscription_t> = in_.cast();

    for (in_sub_new, orig) in wasi_try_mem_ok!(in_new_type_ptr.slice(memory, nsubscriptions))
        .iter()
        .zip(in_origs.iter())
    {
        wasi_try_mem_ok!(in_sub_new.write(types::__wasi_subscription_t {
            userdata: orig.userdata,
            type_: orig.type_,
            u: if orig.type_ == types::__WASI_EVENTTYPE_CLOCK {
                types::__wasi_subscription_u {
                    clock: types::__wasi_subscription_clock_t {
                        clock_id: unsafe { orig.u.clock.clock_id },
                        timeout: unsafe { orig.u.clock.timeout },
                        precision: unsafe { orig.u.clock.precision },
                        flags: unsafe { orig.u.clock.flags },
                    },
                }
            } else {
                types::__wasi_subscription_u {
                    fd_readwrite: unsafe { orig.u.fd_readwrite },
                }
            },
        }));
    }

    // make the call
    let result = syscalls::poll_oneoff(thread, in_new_type_ptr, out_, nsubscriptions, nevents);

    // replace the old values of in, in case the calling code reuses the memory
    let memory = thread.memory();

    for (in_sub, orig) in wasi_try_mem_ok!(in_.slice(memory, nsubscriptions))
        .iter()
        .zip(in_origs.into_iter())
    {
        wasi_try_mem_ok!(in_sub.write(orig));
    }

    result
}
