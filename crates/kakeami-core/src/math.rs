//! Bit-compatible integer hashing, PRNG and periodic value noise.
//! `hash2d` and `Mulberry32` must be bitwise identical to JavaScript.

/// FNV-1a over the low byte of every char (ASCII-only canonical JSON).
pub fn fnv1a(s: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for b in s.bytes() {
        h = (h ^ (b as u32)).wrapping_mul(0x0100_0193);
    }
    h
}

/// `KMath.hash2D(seed, gx, gy)` — exact wrapping u32 arithmetic.
pub fn hash2d(seed: u32, gx: i32, gy: i32) -> u32 {
    let mut h = (seed
        ^ (gx as u32).wrapping_mul(374_761_393)
        ^ (gy as u32).wrapping_mul(668_265_263))
    .wrapping_add(0);
    h = h ^ (h >> 13);
    h = h.wrapping_mul(1_274_126_177);
    h ^ (h >> 16)
}

/// `KMath.mulberry32(seed)` — exact 32-bit PRNG, f64 output.
pub struct Mulberry32 {
    a: u32,
}

impl Mulberry32 {
    pub fn new(seed: u32) -> Self {
        Self { a: seed }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn next_f64(&mut self) -> f64 {
        self.a = self.a.wrapping_add(0x6d2b_79f5);
        let a0 = self.a;
        // JS evaluates the whole right-hand side with the OLD t:
        //   t = Math.imul(t ^ (t >>> 15), t | 1)
        let mut t = (a0 ^ (a0 >> 15)).wrapping_mul(a0 | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        (t ^ (t >> 14)) as f64 * 2.328_306_436_538_696_3e-10
    }
}

/// Periodic value noise backed by a 256-entry permutation repeated to 1024.
pub struct ValueNoise2D {
    perm: [u8; 1024],
    period_x: u32,
    period_y: u32,
}

impl ValueNoise2D {
    pub fn new(seed: u32, period_x: u32, period_y: u32) -> Self {
        let px = if period_x > 0 { period_x.min(512) } else { 256 };
        let py = if period_y > 0 { period_y.min(512) } else { 256 };
        let mut rng = Mulberry32::new(seed ^ 0x9e37_79b9);
        let mut p = [0u8; 256];
        for (i, v) in p.iter_mut().enumerate() {
            *v = i as u8;
        }
        for i in (1..256).rev() {
            let j = (rng.next_f64() * ((i + 1) as f64)) as usize;
            p.swap(i, j);
        }
        let mut perm = [0u8; 1024];
        for (i, v) in perm.iter_mut().enumerate() {
            *v = p[i & 255];
        }
        Self { perm, period_x: px, period_y: py }
    }

    pub fn sample(&self, x: f64, y: f64) -> f64 {
        let ix = x.floor();
        let iy = y.floor();
        let fx = x - ix;
        let fy = y - iy;
        let u = fx * fx * fx * (fx * (fx * 6.0 - 15.0) + 10.0);
        let v = fy * fy * fy * (fy * (fy * 6.0 - 15.0) + 10.0);

        let ix0 = mod_i(ix as i64, self.period_x as i64) as usize;
        let ix1 = mod_i(ix as i64 + 1, self.period_x as i64) as usize;
        let iy0 = mod_i(iy as i64, self.period_y as i64) as usize;
        let iy1 = mod_i(iy as i64 + 1, self.period_y as i64) as usize;

        let p0 = self.perm[ix0] as usize;
        let p1 = self.perm[ix1] as usize;
        let a = self.perm[p0 + iy0] as f64;
        let b = self.perm[p1 + iy0] as f64;
        let c = self.perm[p0 + iy1] as f64;
        let d = self.perm[p1 + iy1] as f64;

        let x1 = a + u * (b - a);
        let x2 = c + u * (d - c);
        (x1 + v * (x2 - x1)) * (1.0 / 255.0)
    }
}

#[inline]
pub fn mod_i(a: i64, n: i64) -> i64 {
    ((a % n) + n) % n
}

/// Exact JavaScript floating-point modulo `((x % n) + n) % n`.
#[inline]
pub fn mod_f64(x: f64, n: f64) -> f64 {
    ((x % n) + n) % n
}

/// Exact JavaScript `Math.round(x)` semantics: rounds half towards +infinity
/// (`-0.5 -> 0.0`, `-1.5 -> -1.0`, `0.5 -> 1.0`), unlike Rust `f64::round()`.
#[inline]
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Shibufude v3 deterministic Murmur3 fmix32 hash (`src/03-raster.js` `hashU32`).
#[inline]
pub fn hash_u32(n: u32) -> f64 {
    let mut v = n ^ 0x9e37_79b9;
    v = (v ^ (v >> 16)).wrapping_mul(0x85eb_ca6b);
    v = (v ^ (v >> 13)).wrapping_mul(0xc2b2_ae35);
    ((v ^ (v >> 16)) as f64) * 2.328_306_436_538_696_3e-10
}

#[inline]
pub fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo { lo } else if v > hi { hi } else { v }
}

#[inline]
pub fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    if a == b {
        return if x < a { 0.0 } else { 1.0 };
    }
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
