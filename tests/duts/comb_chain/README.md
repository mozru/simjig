# comb_chain

A purely combinational path through four dependent continuous assigns:
`s1 = a + 1`, `s2 = s1 ^ 8'h5a`, `s3 = rotl1(s2)`, `y = s3 + b`, and `y_eq = (y == b)`.
A write to `a` must settle through every stage within one eval.

- Top: `comb_chain`. Sources: `comb_chain.sv`. The internal nets `s1`..`s3` are read by
  the conformance test (cocotb builds with `--public-flat-rw`).
- Conformance: `tests/conformance/tb/test_comb_chain.py` (`comb_settle`).
