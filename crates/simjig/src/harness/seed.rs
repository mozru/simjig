//! Seeds: one root seed per test run, and independent named streams derived from it.
//!
//! The root seed comes from `SIMJIG_SEED` when it is set, and otherwise from a hash of the
//! test's name, so every test is deterministic by default and different tests explore
//! different values. A named stream's seed is `mix(root ^ mix(fnv1a(path)))`: adding a
//! stream, or drawing more values from one, never changes another stream (DECISION §2
//! row 3, the seed tree).
//!
//! The generator is xoshiro256** seeded through `SplitMix64`, as its authors recommend
//! (<https://prng.di.unimi.it/>). It is written here (40 lines) instead of taken from a
//! crate so its output is pinned forever: a seed printed in a failure report must replay
//! the same values after any dependency update.

use std::fmt;

use crate::kernel::recorder::Fnv;

/// Where a run's root seed came from.
///
/// `#[non_exhaustive]`, like the other public enums that may grow: a `match` outside
/// `simjig` needs a wildcard arm.
///
/// ```compile_fail,E0004
/// fn from_env(source: simjig::SeedSource) -> bool {
///     match source {
///         simjig::SeedSource::Env => true,
///         simjig::SeedSource::TestName => false,
///     }
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SeedSource {
    /// The `SIMJIG_SEED` environment variable.
    Env,
    /// A hash of the package and test name (the default).
    TestName,
}

impl SeedSource {
    /// `"env"` or `"test-name"`, as written to `summary.json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::TestName => "test-name",
        }
    }
}

/// The root seed of a test run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Seed {
    value: u64,
    source: SeedSource,
}

impl Seed {
    /// A seed with a known value and origin.
    #[must_use]
    pub const fn new(value: u64, source: SeedSource) -> Self {
        Self { value, source }
    }

    /// The seed derived from a test's identity when `SIMJIG_SEED` is not set.
    #[must_use]
    pub fn from_test_name(package: &str, test: &str) -> Self {
        let mut h = Fnv::default();
        h.write(package.as_bytes());
        h.write(b"::");
        h.write(test.as_bytes());
        Self::new(mix64(h.finish()), SeedSource::TestName)
    }

    /// The seed value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.value
    }

    /// Where the seed came from.
    #[must_use]
    pub const fn source(self) -> SeedSource {
        self.source
    }

    /// The stream named `path` (for example `"stim"` or `"tb.axis_in.ready"`).
    ///
    /// The same root and path always give the same stream; different paths give
    /// independent streams.
    #[must_use]
    pub fn rng(self, path: &str) -> Rng {
        let mut h = Fnv::default();
        h.write(path.as_bytes());
        Rng::from_seed(mix64(self.value ^ mix64(h.finish())))
    }

    /// The seed passed to Verilator as `+verilator+seed+<n>`: derived from the stream
    /// `"verilator"` and kept in `1..=i32::MAX`, because Verilator stores it in an `int`
    /// and treats 0 as "pick a random seed" (`verilated.cpp`, 5.052).
    #[must_use]
    pub fn verilator_seed(self) -> u32 {
        positive_int(&mut self.rng("verilator"))
    }

    /// The seed model `model` (its index in the simulation, from 0) draws its random
    /// initial values from ([`InitValues::Random`](crate::InitValues)): the stream
    /// `"init.model-<model>"`, kept in `1..=i32::MAX` because the shim passes it to
    /// `VerilatedContext::randSeed`, an `int`. Each model has its own, so two instances of
    /// one design start apart, while they share [`verilator_seed`](Self::verilator_seed).
    #[must_use]
    pub fn init_seed(self, model: usize) -> u32 {
        positive_int(&mut self.rng(&format!("init.model-{model}")))
    }
}

/// The next value of `rng` as a positive C `int`, `1..=i32::MAX`.
fn positive_int(rng: &mut Rng) -> u32 {
    (rng.next_u32() & 0x7fff_ffff).max(1)
}

/// Prints the value the way `SIMJIG_SEED` accepts it: `0x` and 16 hex digits.
impl fmt::Display for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.value)
    }
}

/// Parses a `SIMJIG_SEED` value: `0x`-prefixed hex or decimal, `_` separators allowed.
///
/// # Errors
/// A message naming the rejected text and the accepted forms.
pub(crate) fn parse_seed(text: &str) -> Result<u64, String> {
    let trimmed = text.trim().replace('_', "");
    let parsed = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => trimmed.parse(),
    };
    parsed.map_err(|e| {
        format!(
            "SIMJIG_SEED={text:?} is not a seed ({e}); use 0x-prefixed hex (as printed in failure \
             reports, e.g. SIMJIG_SEED=0x9e3779b97f4a7c15) or a decimal integer"
        )
    })
}

/// A deterministic random number generator (xoshiro256**) for one named stream.
///
/// Made by [`TestCtx::rng`](crate::TestCtx::rng) or [`Seed::rng`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    /// A generator whose state is filled from `seed` by `SplitMix64`.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        let mut sm = seed;
        let mut next = || {
            sm = sm.wrapping_add(0x9e37_79b9_7f4a_7c15);
            mix64(sm)
        };
        Self {
            s: [next(), next(), next(), next()],
        }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// The next 32 random bits (the high half of [`next_u64`](Self::next_u64), the
    /// better-mixed half).
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// A uniform value in `0..bound`, without modulo bias (Lemire's method).
    ///
    /// # Panics
    /// If `bound` is 0.
    #[track_caller]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "splits a 128-bit product into its 64-bit halves"
    )]
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "Rng::below(0): the range 0..0 is empty");
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let wide = u128::from(self.next_u64()) * u128::from(bound);
            if (wide as u64) >= threshold {
                return (wide >> 64) as u64;
            }
        }
    }

    /// A fair coin flip.
    pub fn bool(&mut self) -> bool {
        self.next_u64() >> 63 == 1
    }
}

/// The `SplitMix64` finalizer: a bijective mix of 64 bits.
pub(crate) const fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xoshiro_matches_the_reference_implementation() {
        // Reference outputs of xoshiro256** from state {1, 2, 3, 4} (xoshiro256starstar.c).
        let mut rng = Rng { s: [1, 2, 3, 4] };
        let got: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
        assert_eq!(got, [11520, 0, 1_509_978_240, 1_215_971_899_390_074_240]);
    }

    #[test]
    fn splitmix_matches_the_reference_implementation() {
        // splitmix64.c from seed 0: the first output.
        assert_eq!(Rng::from_seed(0).s[0], 0xe220_a839_7b1d_cdaf);
    }

    #[test]
    fn streams_are_stable_and_independent() {
        let seed = Seed::new(42, SeedSource::Env);
        let draw = |path: &str| -> Vec<u64> {
            let mut r = seed.rng(path);
            (0..4).map(|_| r.next_u64()).collect()
        };
        assert_eq!(draw("stim"), draw("stim"));
        assert_ne!(draw("stim"), draw("ready"));
        assert_ne!(draw("stim"), {
            let mut r = Seed::new(43, SeedSource::Env).rng("stim");
            (0..4).map(|_| r.next_u64()).collect::<Vec<_>>()
        });
    }

    #[test]
    fn test_name_seeds_differ_per_test_and_package() {
        let a = Seed::from_test_name("p", "m::a");
        assert_eq!(a, Seed::from_test_name("p", "m::a"));
        assert_ne!(a.value(), Seed::from_test_name("p", "m::b").value());
        assert_ne!(a.value(), Seed::from_test_name("q", "m::a").value());
        assert_eq!(a.source(), SeedSource::TestName);
    }

    #[test]
    fn below_stays_in_range_and_covers_it() {
        let mut rng = Rng::from_seed(7);
        let mut seen = [false; 5];
        for _ in 0..200 {
            let v = rng.below(5);
            seen[usize::try_from(v).unwrap_or(usize::MAX)] = true;
        }
        assert_eq!(seen, [true; 5]);
        assert_eq!(rng.below(1), 0);
    }

    #[test]
    fn verilator_seed_is_a_positive_int() {
        for v in 0..1000 {
            let s = Seed::new(v, SeedSource::Env).verilator_seed();
            assert!((1..=0x7fff_ffff).contains(&s), "{s}");
        }
    }

    #[test]
    fn init_seeds_are_positive_ints_per_model() {
        let seed = Seed::new(9, SeedSource::Env);
        assert_eq!(seed.init_seed(0), seed.init_seed(0));
        assert_ne!(seed.init_seed(0), seed.init_seed(1));
        assert_ne!(
            seed.init_seed(0),
            Seed::new(10, SeedSource::Env).init_seed(0)
        );
        for v in 0..1000 {
            let s = Seed::new(v, SeedSource::Env).init_seed(3);
            assert!((1..=0x7fff_ffff).contains(&s), "{s}");
        }
    }

    #[test]
    fn seeds_print_as_they_parse() {
        let seed = Seed::new(0x9e37_79b9, SeedSource::Env);
        assert_eq!(seed.to_string(), "0x000000009e3779b9");
        assert_eq!(parse_seed(&seed.to_string()), Ok(0x9e37_79b9));
        assert_eq!(parse_seed("1_000"), Ok(1000));
        assert_eq!(parse_seed(" 0XfF "), Ok(255));
        let err = parse_seed("banana").unwrap_err();
        assert!(err.contains("SIMJIG_SEED=\"banana\""), "{err}");
    }
}
