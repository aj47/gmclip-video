//! BLAKE3 (hash mode, 32-byte output), portable, after the BLAKE3 team's reference
//! implementation (CC0/Apache-2.0). Our own because the `blake3` crate runs a build
//! script (C/asm SIMD), which `cargo xt supply` refuses without AJ's audit (SR R-SUP-1);
//! `render_sig` hashes a few kilobytes, so the portable path is plenty. Checked against
//! the official test vectors (`blake3_vectors.txt`).

const OUT_LEN: usize = 32;
const BLOCK_LEN: usize = 64;
const CHUNK_LEN: usize = 1024;

const CHUNK_START: u32 = 1;
const CHUNK_END: u32 = 2;
const PARENT: u32 = 4;
const ROOT: u32 = 8;

const IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];

const PERM: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];

#[allow(clippy::many_single_char_names)]
fn g(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, mx: u32, my: u32) {
    s[a] = s[a].wrapping_add(s[b]).wrapping_add(mx);
    s[d] = (s[d] ^ s[a]).rotate_right(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_right(12);
    s[a] = s[a].wrapping_add(s[b]).wrapping_add(my);
    s[d] = (s[d] ^ s[a]).rotate_right(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_right(7);
}

fn round(s: &mut [u32; 16], m: &[u32; 16]) {
    g(s, 0, 4, 8, 12, m[0], m[1]);
    g(s, 1, 5, 9, 13, m[2], m[3]);
    g(s, 2, 6, 10, 14, m[4], m[5]);
    g(s, 3, 7, 11, 15, m[6], m[7]);
    g(s, 0, 5, 10, 15, m[8], m[9]);
    g(s, 1, 6, 11, 12, m[10], m[11]);
    g(s, 2, 7, 8, 13, m[12], m[13]);
    g(s, 3, 4, 9, 14, m[14], m[15]);
}

fn compress(
    cv: &[u32; 8],
    block: &[u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
) -> [u32; 16] {
    #[allow(clippy::cast_possible_truncation)]
    let mut s = [
        cv[0],
        cv[1],
        cv[2],
        cv[3],
        cv[4],
        cv[5],
        cv[6],
        cv[7],
        IV[0],
        IV[1],
        IV[2],
        IV[3],
        counter as u32,
        (counter >> 32) as u32,
        block_len,
        flags,
    ];
    let mut m = *block;
    for r in 0..7 {
        round(&mut s, &m);
        if r < 6 {
            let mut p = [0u32; 16];
            for (i, &k) in PERM.iter().enumerate() {
                p[i] = m[k];
            }
            m = p;
        }
    }
    for i in 0..8 {
        s[i] ^= s[i + 8];
        s[i + 8] ^= cv[i];
    }
    s
}

fn first8(w: &[u32; 16]) -> [u32; 8] {
    let mut o = [0u32; 8];
    o.copy_from_slice(&w[..8]);
    o
}

fn words(block: &[u8; BLOCK_LEN]) -> [u32; 16] {
    let mut w = [0u32; 16];
    for (i, c) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
    w
}

/// What the last compression of a node needs (so ROOT can be added at the end).
#[derive(Clone, Copy)]
struct Output {
    cv: [u32; 8],
    block: [u32; 16],
    counter: u64,
    block_len: u32,
    flags: u32,
}

impl Output {
    fn chaining_value(&self) -> [u32; 8] {
        first8(&compress(
            &self.cv,
            &self.block,
            self.counter,
            self.block_len,
            self.flags,
        ))
    }

    fn root_hash(&self) -> [u8; OUT_LEN] {
        let w = compress(&self.cv, &self.block, 0, self.block_len, self.flags | ROOT);
        let mut out = [0u8; OUT_LEN];
        for (i, word) in w.iter().take(8).enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

#[derive(Clone)]
struct ChunkState {
    cv: [u32; 8],
    counter: u64,
    block: [u8; BLOCK_LEN],
    block_len: usize,
    blocks_done: usize,
}

impl ChunkState {
    fn new(counter: u64) -> Self {
        Self {
            cv: IV,
            counter,
            block: [0; BLOCK_LEN],
            block_len: 0,
            blocks_done: 0,
        }
    }

    fn len(&self) -> usize {
        BLOCK_LEN * self.blocks_done + self.block_len
    }

    fn start_flag(&self) -> u32 {
        if self.blocks_done == 0 {
            CHUNK_START
        } else {
            0
        }
    }

    fn update(&mut self, mut input: &[u8]) {
        while !input.is_empty() {
            if self.block_len == BLOCK_LEN {
                let w = words(&self.block);
                self.cv = first8(&compress(
                    &self.cv,
                    &w,
                    self.counter,
                    BLOCK_LEN as u32,
                    self.start_flag(),
                ));
                self.blocks_done += 1;
                self.block = [0; BLOCK_LEN];
                self.block_len = 0;
            }
            let take = (BLOCK_LEN - self.block_len).min(input.len());
            self.block[self.block_len..self.block_len + take].copy_from_slice(&input[..take]);
            self.block_len += take;
            input = &input[take..];
        }
    }

    fn output(&self) -> Output {
        #[allow(clippy::cast_possible_truncation)]
        Output {
            cv: self.cv,
            block: words(&self.block),
            counter: self.counter,
            block_len: self.block_len as u32,
            flags: self.start_flag() | CHUNK_END,
        }
    }
}

fn parent(left: [u32; 8], right: [u32; 8]) -> Output {
    let mut block = [0u32; 16];
    block[..8].copy_from_slice(&left);
    block[8..].copy_from_slice(&right);
    Output {
        cv: IV,
        block,
        counter: 0,
        block_len: BLOCK_LEN as u32,
        flags: PARENT,
    }
}

/// An incremental BLAKE3 hasher.
#[derive(Clone)]
pub struct Hasher {
    chunk: ChunkState,
    stack: Vec<[u32; 8]>,
}

impl std::fmt::Debug for Hasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("blake3::Hasher").finish_non_exhaustive()
    }
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            chunk: ChunkState::new(0),
            stack: Vec::new(),
        }
    }

    fn push_chunk_cv(&mut self, mut cv: [u32; 8], mut total_chunks: u64) {
        while total_chunks & 1 == 0 {
            if let Some(left) = self.stack.pop() {
                cv = parent(left, cv).chaining_value();
            }
            total_chunks >>= 1;
        }
        self.stack.push(cv);
    }

    pub fn update(&mut self, mut input: &[u8]) -> &mut Self {
        while !input.is_empty() {
            if self.chunk.len() == CHUNK_LEN {
                let cv = self.chunk.output().chaining_value();
                let total = self.chunk.counter + 1;
                self.push_chunk_cv(cv, total);
                self.chunk = ChunkState::new(total);
            }
            let take = (CHUNK_LEN - self.chunk.len()).min(input.len());
            self.chunk.update(&input[..take]);
            input = &input[take..];
        }
        self
    }

    #[must_use]
    pub fn finalize(&self) -> [u8; OUT_LEN] {
        let mut out = self.chunk.output();
        for left in self.stack.iter().rev() {
            out = parent(*left, out.chaining_value());
        }
        out.root_hash()
    }
}

/// BLAKE3 of `data`.
#[must_use]
pub fn hash(data: &[u8]) -> [u8; OUT_LEN] {
    Hasher::new().update(data).finalize()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn official_vectors() {
        let mut n = 0;
        for line in include_str!("blake3_vectors.txt")
            .lines()
            .filter(|l| !l.starts_with('#'))
        {
            let (len, want) = line.split_once(' ').unwrap();
            let len: usize = len.parse().unwrap();
            let input: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(hex(&hash(&input)), want, "len {len}");
            // Incremental, in awkward pieces, gives the same digest.
            let mut h = Hasher::new();
            for piece in input.chunks(97) {
                h.update(piece);
            }
            assert_eq!(hex(&h.finalize()), want, "len {len} incremental");
            n += 1;
        }
        assert_eq!(n, 35);
    }
}
