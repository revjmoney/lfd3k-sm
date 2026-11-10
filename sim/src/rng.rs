//! xorshift64* pseudo random numbers. Deterministic, no dependencies.

#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    /// A zero seed would stick at zero, so it is replaced by a fixed constant.
    pub fn new(seed: u64) -> Rng {
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// True with probability `p_x1000 / 1000`. Always draws one number, so
    /// the sequence does not depend on `p_x1000`.
    pub fn chance(&mut self, p_x1000: u32) -> bool {
        self.next_u32() % 1000 < p_x1000
    }
}
