//! Bit-exact port of NumPy's `default_rng` primitives used by the classifier.
//!
//! The v2 classifier's RANSAC draws must match `numpy.random.default_rng(0)`
//! exactly, or `plane_count`/`plane_area` (and therefore type/confidence)
//! diverge. This module ports the pieces the RANSAC uses:
//!
//! * `SeedSequence` (NumPy `bit_generator.pyx`: hashmix/mix/generate_state),
//! * `PCG64` state, seeding and `next64`/`next32` buffering
//!   (`numpy/random/src/pcg64`),
//! * `Generator.integers` (Lemire bounded draws, 32- and 64-bit variants),
//! * `Generator.choice(..., replace=False)` without `p`: NumPy's Floyd hash-set
//!   path and the tail-shuffle path, including `_shuffle_int`.
//!
//! Tests pin the outputs captured from NumPy 2.5.4 (see
//! `tests/fixtures/rng_vectors.json`).

const INIT_A: u32 = 0x43b0_d7e5;
const MULT_A: u32 = 0x931e_8875;
const INIT_B: u32 = 0x8b51_f9dd;
const MULT_B: u32 = 0x58f3_8ded;
const MIX_MULT_L: u32 = 0xca01_f9dd;
const MIX_MULT_R: u32 = 0x4973_f715;
const XSHIFT: u32 = 16;
const MASK32: u32 = 0xFFFF_FFFF;
const DEFAULT_POOL_SIZE: usize = 4;

const PCG_MULT_HIGH: u64 = 2_549_297_995_355_413_924;
const PCG_MULT_LOW: u64 = 4_865_540_595_714_422_341;

fn hashmix(value: u32, hash_const: &mut u32) -> u32 {
    let mut value = value ^ *hash_const;
    *hash_const = hash_const.wrapping_mul(MULT_A);
    value = value.wrapping_mul(*hash_const);
    value ^= value >> XSHIFT;
    value
}

fn mix(x: u32, y: u32) -> u32 {
    let mut result = MIX_MULT_L
        .wrapping_mul(x)
        .wrapping_sub(MIX_MULT_R.wrapping_mul(y));
    result ^= result >> XSHIFT;
    result
}

fn mix_entropy(mixer: &mut [u32], entropy: &[u32]) {
    let mut hash_const = INIT_A;
    for i in 0..mixer.len() {
        let value = if i < entropy.len() { entropy[i] } else { 0 };
        mixer[i] = hashmix(value, &mut hash_const);
    }
    for i_src in 0..mixer.len() {
        for i_dst in 0..mixer.len() {
            if i_src != i_dst {
                let h = hashmix(mixer[i_src], &mut hash_const);
                mixer[i_dst] = mix(mixer[i_dst], h);
            }
        }
    }
    for &word in entropy.iter().skip(mixer.len()) {
        for dst in mixer.iter_mut() {
            let h = hashmix(word, &mut hash_const);
            *dst = mix(*dst, h);
        }
    }
}

/// NumPy's `SeedSequence` (pool size 4) for a non-negative integer seed.
#[derive(Clone, Debug)]
pub struct SeedSequence {
    pub pool: [u32; DEFAULT_POOL_SIZE],
}

impl SeedSequence {
    pub fn new(seed: u64) -> Self {
        let mut entropy = Vec::new();
        if seed == 0 {
            entropy.push(0);
        } else {
            let mut n = seed;
            while n > 0 {
                entropy.push((n & u64::from(MASK32)) as u32);
                n >>= 32;
            }
        }
        let mut pool = [0u32; DEFAULT_POOL_SIZE];
        mix_entropy(&mut pool, &entropy);
        Self { pool }
    }

    /// `generate_state(n_words, np.uint32)`.
    pub fn generate_state_u32(&self, n_words: usize) -> Vec<u32> {
        let mut hash_const = INIT_B;
        let mut out = Vec::with_capacity(n_words);
        for i in 0..n_words {
            let mut value = self.pool[i % DEFAULT_POOL_SIZE];
            value ^= hash_const;
            hash_const = hash_const.wrapping_mul(MULT_B);
            value = value.wrapping_mul(hash_const);
            value ^= value >> XSHIFT;
            out.push(value);
        }
        out
    }

    /// `generate_state(n_words, np.uint64)` (little-endian word pairing).
    pub fn generate_state_u64(&self, n_words: usize) -> Vec<u64> {
        let words = self.generate_state_u32(n_words * 2);
        words
            .chunks_exact(2)
            .map(|pair| u64::from(pair[0]) | (u64::from(pair[1]) << 32))
            .collect()
    }
}

/// NumPy `PCG64` (PCG XSL RR 128/64).
#[derive(Clone, Debug)]
pub struct Pcg64 {
    state: u128,
    inc: u128,
    has_uint32: bool,
    uinteger: u32,
}

impl Pcg64 {
    pub fn new(seed: u64) -> Self {
        let seq = SeedSequence::new(seed);
        let words = seq.generate_state_u64(4);
        let initstate = (u128::from(words[0]) << 64) | u128::from(words[1]);
        let initseq = (u128::from(words[2]) << 64) | u128::from(words[3]);
        let mut rng = Self {
            state: 0,
            inc: 0,
            has_uint32: false,
            uinteger: 0,
        };
        rng.set_seed(initstate, initseq);
        rng
    }

    pub fn state(&self) -> (u128, u128) {
        (self.state, self.inc)
    }

    fn set_seed(&mut self, initstate: u128, initseq: u128) {
        self.state = 0;
        self.inc = (initseq << 1) | 1;
        self.step();
        self.state = self.state.wrapping_add(initstate);
        self.step();
        self.has_uint32 = false;
        self.uinteger = 0;
    }

    #[inline]
    fn step(&mut self) {
        let mult = (u128::from(PCG_MULT_HIGH) << 64) | u128::from(PCG_MULT_LOW);
        self.state = self.state.wrapping_mul(mult).wrapping_add(self.inc);
    }

    pub fn next_u64(&mut self) -> u64 {
        self.step();
        let high = (self.state >> 64) as u64;
        let low = self.state as u64;
        let rot = (high >> 58) as u32;
        (high ^ low).rotate_right(rot)
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.has_uint32 {
            self.has_uint32 = false;
            return self.uinteger;
        }
        let next = self.next_u64();
        self.has_uint32 = true;
        self.uinteger = (next >> 32) as u32;
        next as u32
    }

    /// `random_bounded_uint64(..., off=0, rng=bound-1, use_masked=False)`.
    pub fn bounded(&mut self, bound: u64) -> u64 {
        // NumPy returns `off` for rng == 0 without drawing a word.
        if bound <= 1 {
            return 0;
        }
        let rng = bound - 1;
        if rng <= u64::from(MASK32 as u32) {
            if rng == u64::from(MASK32) {
                return u64::from(self.next_u32());
            }
            return u64::from(self.bounded_lemire_u32(rng as u32));
        }
        if rng == u64::MAX {
            return self.next_u64();
        }
        self.bounded_lemire_u64(rng)
    }

    fn bounded_lemire_u32(&mut self, rng: u32) -> u32 {
        let rng_excl = u64::from(rng) + 1;
        let mut m = u64::from(self.next_u32()).wrapping_mul(rng_excl);
        let mut leftover = (m & u64::from(MASK32)) as u32;
        if leftover < rng_excl as u32 {
            let threshold = (u32::MAX - rng) % rng_excl as u32;
            while leftover < threshold {
                m = u64::from(self.next_u32()).wrapping_mul(rng_excl);
                leftover = (m & u64::from(MASK32)) as u32;
            }
        }
        (m >> 32) as u32
    }

    fn bounded_lemire_u64(&mut self, rng: u64) -> u64 {
        let rng_excl = rng.wrapping_add(1);
        let x = self.next_u64();
        let m = u128::from(x) * u128::from(rng_excl);
        let mut leftover = m as u64;
        if leftover < rng_excl {
            let threshold = (u64::MAX - rng) % rng_excl;
            while leftover < threshold {
                let x = self.next_u64();
                let m = u128::from(x) * u128::from(rng_excl);
                leftover = m as u64;
                if leftover >= threshold {
                    return (m >> 64) as u64;
                }
            }
        }
        (m >> 64) as u64
    }

    /// `Generator.integers(0, high, dtype=int64)`, one draw.
    pub fn integers(&mut self, high: u64) -> u64 {
        self.bounded(high)
    }

    /// `Generator.choice(n, size=size, replace=False)` without probabilities.
    ///
    /// `shuffle=true` matches the classifier's call. The implementation picks
    /// NumPy's two paths: a tail shuffle for large populations, Floyd's
    /// hash-set algorithm otherwise.
    pub fn choice_without_replacement(&mut self, n: u64, size: usize) -> Vec<i64> {
        assert!(size as u64 <= n, "sample larger than population");
        if size == 0 {
            return Vec::new();
        }
        let shuffle = true;
        if n > 10_000 && (size as u64) > (n / 50) {
            let mut idx: Vec<i64> = (0..n as i64).collect();
            let first = std::cmp::max(n as i64 - size as i64, 1);
            self.shuffle_int(&mut idx, first);
            return idx[(n as usize - size)..].to_vec();
        }

        let mut idx = vec![0i64; size];
        let set_size = (1.2 * size as f64) as u64;
        let mask = gen_mask(set_size);
        let table_len = (mask + 1) as usize;
        let mut hash_set = vec![u64::MAX; table_len];
        let pop = n as i64;
        let size_i = size as i64;
        for j in (pop - size_i)..pop {
            let val = self.bounded(j as u64 + 1);
            let mut loc = (val & mask) as usize;
            while hash_set[loc] != u64::MAX && hash_set[loc] != val {
                loc = (loc + 1) & mask as usize;
            }
            if hash_set[loc] == u64::MAX {
                hash_set[loc] = val;
                idx[(j - pop + size_i) as usize] = val as i64;
            } else {
                let mut loc = (j as u64 & mask) as usize;
                while hash_set[loc] != u64::MAX {
                    loc = (loc + 1) & mask as usize;
                }
                hash_set[loc] = j as u64;
                idx[(j - pop + size_i) as usize] = j;
            }
        }
        if shuffle {
            self.shuffle_int(&mut idx, 1);
        }
        idx
    }

    fn shuffle_int(&mut self, data: &mut [i64], first: i64) {
        let n = data.len() as i64;
        for i in (first..n).rev() {
            let j = self.bounded(i as u64 + 1) as usize;
            data.swap(j, i as usize);
        }
    }
}

/// `_gen_mask`: the smallest `2^k - 1` that is `>= x`.
fn gen_mask(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut mask = 0u64;
    while mask < x {
        mask = (mask << 1) | 1;
        if mask == u64::MAX {
            break;
        }
    }
    mask
}
