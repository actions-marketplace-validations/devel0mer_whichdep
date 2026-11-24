//! Stable, dependency-free fingerprints used for experiment/cache identity.
//!
//! This is deliberately not a cryptographic hash. Trial-file names are only
//! indexes: the store compares the complete encoded composite key before
//! accepting an entry. Source/oracle fingerprints are still probabilistic, so
//! callers with volatile or adversarial inputs should disable resume.

#[derive(Clone, Debug)]
pub struct StableHasher {
    a: u64,
    b: u64,
}

impl StableHasher {
    pub fn new() -> Self {
        Self {
            a: 0xcbf29ce484222325,
            b: 0x84222325cbf29ce4,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.a ^= u64::from(*byte);
            self.a = self.a.wrapping_mul(0x100000001b3);
            self.b ^= u64::from(*byte);
            self.b = self.b.wrapping_mul(0x100000001b3);
            self.b ^= self.b >> 29;
        }
    }

    pub fn finish(&self) -> String {
        format!("{:016x}{:016x}", self.a, self.b)
    }
}

impl Default for StableHasher {
    fn default() -> Self {
        Self::new()
    }
}

pub fn hash(bytes: &[u8]) -> String {
    let mut h = StableHasher::new();
    h.update(bytes);
    h.finish()
}
