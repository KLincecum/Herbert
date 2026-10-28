import { dispatch } from "./plugin";

// These historical export names are ABI 1. Keep byte and pointer handling here;
// the example itself works with JSON and needs no direct memory operations.
export function hoover_abi(): u32 {
  return 1;
}

export function hoover_alloc(length: u32): u32 {
  return <u32>heap.alloc(length);
}

export function hoover_free(pointer: u32, _length: u32): void {
  // Only raw heap buffers are released here. Never heap.free a managed object.
  heap.free(pointer);
}

export function hoover_call(pointer: u32, length: u32): u64 {
  const input = String.UTF8.decodeUnsafe(pointer, length);
  const reply = String.UTF8.encode(dispatch(input), false);
  const output = heap.alloc(reply.byteLength);
  memory.copy(output, changetype<usize>(reply), reply.byteLength);
  return ((<u64>output) << 32) | (<u64>reply.byteLength);
}

// A trap is contained by the host. This replacement removes env.abort imports,
// so the module gains no ambient logging, filesystem or environment capability.
export function abort(
  _message: string | null,
  _file: string | null,
  _line: u32,
  _column: u32,
): void {
  unreachable();
}
