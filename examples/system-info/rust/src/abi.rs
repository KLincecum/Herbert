//! The four exports are Herbert's ABI 1 byte bridge.
//!
//! The historical `hoover_` export names stay fixed for compatibility. A plugin
//! receives UTF-8 JSON and returns UTF-8 JSON. It has no filesystem or network
//! imports. Keep pointer handling here so the example logic needs no unsafe code.

#[unsafe(no_mangle)]
pub extern "C" fn hoover_abi() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn hoover_alloc(length: u32) -> u32 {
    let bytes = vec![0u8; length as usize].into_boxed_slice();
    Box::into_raw(bytes) as *mut u8 as u32
}

/// The host returns the same pointer and length that this module allocated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hoover_free(pointer: u32, length: u32) {
    let slice = std::ptr::slice_from_raw_parts_mut(pointer as *mut u8, length as usize);
    unsafe { drop(Box::from_raw(slice)) };
}

/// Pack the output pointer in the high 32 bits and its byte length in the low bits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hoover_call(pointer: u32, length: u32) -> u64 {
    let input = unsafe { std::slice::from_raw_parts(pointer as *const u8, length as usize) };
    let output = crate::dispatch(input).into_boxed_slice();
    // wasm32's address space is the ABI bound. This does not limit native files.
    let output_length = u32::try_from(output.len()).expect("Output must fit wasm32 memory");
    let output_pointer = Box::into_raw(output) as *mut u8 as u32;
    ((output_pointer as u64) << 32) | output_length as u64
}
