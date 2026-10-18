//! CPython `random.Random(seed)` 的 **MT19937 逐字复刻**。
//!
//! 为什么需要：`emotion_engine.branch_probs(seed=N)` 用 `random.Random(N).random()`
//! 抽 r，设计目标是「0 全局随机、可重放」。要与 Python 输出**逐位一致**，
//! 必须复刻 CPython 的 seed 初始化（init_by_array）与 random() 的 53-bit 组合方式。
//!
//! 对齐依据（CPython `_randommodule.c` / `random.py`）：
//!   * int seed → 取绝对值，转 32-bit little-endian words；bits=0 时 keyused=1
//!     `keyused = (bits-1)/32 + 1`
//!   * `init_by_array(key, keyused)`（标准 MT19937 初始化）
//!   * `random()` = ((a>>5)*67108864.0 + (b>>6)) / 9007199254740992.0
//!     其中 a、b 是连续两次 `genrand_uint32()`

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

pub struct PyRandom {
    mt: [u32; N],
    mti: usize,
}

impl PyRandom {
    /// 等价 `random.Random(seed)`（seed 为整数）。
    pub fn from_int(seed: i64) -> Self {
        let mut r = PyRandom { mt: [0; N], mti: N + 1 };
        // abs(seed) → 32-bit little-endian words
        let mag: u64 = seed.unsigned_abs();
        let bits = if mag == 0 { 0 } else { 64 - mag.leading_zeros() as usize };
        let keyused = if bits == 0 { 1 } else { (bits - 1) / 32 + 1 };
        let mut key = Vec::with_capacity(keyused);
        for i in 0..keyused {
            key.push(((mag >> (32 * i)) & 0xffff_ffff) as u32);
        }
        r.init_by_array(&key);
        r
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..N {
            let prev = self.mt[i - 1];
            self.mt[i] = 1812433253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        self.mti = N;
    }

    fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19650218);
        let mut i = 1usize;
        let mut j = 0usize;
        let mut k = N.max(key.len());
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i]
                ^ (prev ^ (prev >> 30)).wrapping_mul(1664525))
            .wrapping_add(key[j])
            .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
            k -= 1;
        }
        let mut k = N - 1;
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i]
                ^ (prev ^ (prev >> 30)).wrapping_mul(1566083941))
            .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            k -= 1;
        }
        self.mt[0] = 0x8000_0000;
    }

    /// 等价 `genrand_uint32()`。
    pub fn genrand_uint32(&mut self) -> u32 {
        if self.mti >= N {
            if self.mti == N + 1 {
                self.init_genrand(5489); // 默认种子（不应触发）
            }
            for kk in 0..(N - M) {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
            }
            for kk in (N - M)..(N - 1) {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M - N] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
            }
            let y = (self.mt[N - 1] & UPPER_MASK) | (self.mt[0] & LOWER_MASK);
            self.mt[N - 1] = self.mt[M - 1] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// 等价 `random.random()`（53-bit 精度浮点，区间 [0,1)）。
    pub fn random(&mut self) -> f64 {
        let a = (self.genrand_uint32() >> 5) as f64; // 27 bits
        let b = (self.genrand_uint32() >> 6) as f64; // 26 bits
        (a * 67108864.0 + b) / 9007199254740992.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 CPython 对拍（值由 python3 -c 实测得到，见测试报告）。
    #[test]
    fn matches_cpython_int_seed() {
        // python3 -c "import random;print(repr(random.Random(42).random()))"
        // → 0.6394267984578837
        let mut r = PyRandom::from_int(42);
        let v = r.random();
        assert!((v - 0.6394267984578837).abs() < 1e-15, "got {v}");
    }

    #[test]
    fn matches_cpython_second_value() {
        // 第二个值：random.Random(42).random() 连调两次
        let mut r = PyRandom::from_int(42);
        let _ = r.random();
        let v2 = r.random();
        assert!((v2 - 0.025010755222666936).abs() < 1e-15, "got {v2}");
    }

    #[test]
    fn matches_cpython_seed_zero() {
        // random.Random(0).random() → 0.8444218515250481
        let mut r = PyRandom::from_int(0);
        assert!((r.random() - 0.8444218515250481).abs() < 1e-15);
    }
}
