# simjig-bits notes

## Literal inference (the API decision)

`sig.set(v: impl Into<T>)` must accept `set(0x5a)`. Each value type implements `From` for
exactly one integer type, `u128`. With one integral candidate, rustc resolves the
literal's `{integer}` variable to `u128`. With several (`From<u8>`, `From<u32>`, ...), the
variable stays ambiguous and falls back to `i32`. Then `set(0xdead_beef)` is a
compile error (the literal overflows `i32`), and every width above 31 bits loses literals.

Results:
- Every literal up to 128 bits works at every width. A negative literal is a compile error
  (E0277, `u128: Neg`).
- A typed `u8`/`u16`/`u32`/`u64`/`i*` variable converts with `TryFrom`. That is the cost:
  `Bits::<8>::from(x_u8)` does not exist, even though it could never fail.
- `TryFrom<u128>` cannot be implemented because the std blanket impl (from `From<u128>`)
  already provides it, with `Infallible`. Use `Bits::try_new(u128)`.
- `Into::into` is `#[track_caller]` in std (checked: the panic location goes through
  `Into` to the caller), so `From<u128>` panics at the user's `set(...)` line as long as
  `set` is also `#[track_caller]`. `tests/it/literals.rs` checks the file and line.

**Gap against DECISION.md §6.** The decision says the panic names *the signal* and its
width. Through `impl Into<T>`, the conversion runs before `set` can add the signal name,
so the message names the width (`Bits<8>: value 0x15a does not fit in 8 bits (maximum
0xff)`) and the location points at the call. To also name the signal, `set` would need its
own conversion trait (`fn try_into_value(self) -> Result<T, OutOfRange>`, implemented for
`u128`, `bool` and `T` itself). Literal inference would be unchanged. That is a decision
for the signals owner and the director, so it is not built here.

## Compile-time width checks

The checks are inline-const / associated-const assertions: `1 <= N <= 128` for `Bits`,
`N > 128` and `W == N.div_ceil(32)` for `WideBits`, and `M >= N` / `M <= N` for
`zext`/`sext`/`trunc`. They are post-monomorphization errors (E0080), so **`cargo check`
does not report them; `cargo build` and `cargo test` do.** A type like `Bits<0>` can be
named but never constructed. The `compile_fail` doctests cover each check. Stable rustdoc
ignores the error code on `compile_fail`, so I confirmed by hand, in a scratch crate,
that each one fails with the intended message.

## `Packed` for `Bits<N>` is 128 concrete impls

`Raw` depends on `N` (u8/u16/u32/u64/[u32; 3]/[u32; 4]). Stable Rust cannot compute a
type from a const, so a macro generates `impl Packed for Bits<1>` ... `Bits<128>`.
Consequence: generic code over `N` must add the bound `Bits<N>: Packed`. The other
design, a helper trait `Width<N>: RawFor`, needs a bound that is just as visible, so I
chose the direct one.

`Bits<65..=128>` uses `[u32; 3|4]` as raw storage, because Verilator stores 65+ bits as
`VlWide`, even though the value is held in a `u128`.

## Contract issue for generated enums

`Packed::from_raw` is total (it never fails). A generated enum with unused encodings
therefore has to represent unknown encodings, for example with an `Unknown(raw)` variant.
Otherwise the trait needs a fallible decode. This is flagged for the codegen owner.

## Deferred

- `SBits<N>`: deferred. `sext` covers the immediate need. A signed type duplicates every
  operator and adds signed comparison and display. That is not small.
- `Logic<N>`: not started. Nothing here assumes 2-state beyond the names. The `Packed`
  contract describes raw 2-state storage. 4-state values will go through
  `get_planes`/`set_planes` (agentic-rust §9b), not through `Packed::Raw`.
- Shifts, `widening_add`, `cat!`/`sv!`, and slices of more than 128 bits out of a
  `WideBits` (`slice` returns `Bits<M>`, so `M <= 128`).
- `WideBits` slice and deposit run bit by bit (at most 128 iterations). Simple and
  obviously correct. Optimize when a benchmark shows it matters.

## Bugs found by the property tests

- `WideBits` compared with `u128` compared word slices of different lengths
  lexicographically (`[w0..w6]` against `[w0..w3]`), so equal values compared as
  `Greater`. Fixed by widening the integer first. `words::cmp` now takes two arrays of
  the same length, so the bug cannot come back.

Mutation checks (by hand, then reverted): removing the sign fill in `Bits::sext` fails 11
tests, and starting the wide adder with carry-in 1 fails 2.
