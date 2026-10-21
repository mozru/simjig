# Async speedup check

The async flow before (`fffbbd6`) and after the async performance work, measured with
`bench/run.sh` on an idle Apple M6 (load 1.9-2.2). The two builds were run alternately,
twice each, to cancel out drift between sessions. Median ns per cycle, fifo at 1M cycles
(10 runs each), mid at 100k cycles (6 runs each). Both builds produced identical pushes,
pops and result hashes.

| design | variant | before | after | throughput |
|---|---|---:|---:|---:|
| fifo | clock only | 159.0 | 122.9 | 1.29x |
| fifo | monitor only | 171.3 | 132.6 | 1.29x |
| fifo | full | 299.6 | 225.9 | 1.33x |
| mid | clock only | 1360.5 | 1337.8 | 1.02x |
| mid | monitor only | 1374.6 | 1345.3 | 1.02x |
| mid | full | 1512.2 | 1439.6 | 1.05x |

`*-simjig-N.jsonl` are the before runs (the main checkout), `*-perf30-N.jsonl` the after runs.
