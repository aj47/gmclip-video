// A QR code encoder (ISO/IEC 18004: byte mode, error correction M, versions 1–40, the
// mask with the lowest penalty), for showing a review link to a phone camera (SPEC §5.14.1,
// A.03). Written here so the app needs no new dependency; nothing leaves the page. The
// result is a square of booleans (true = dark module), drawn by the caller on a canvas
// with a 4-module quiet zone.

/** Codewords of error correction per block, ECC level M, by version (index 0 unused). */
const ECC_PER_BLOCK_M = [
  -1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
  28, 28, 28,
];
/** Error correction blocks, ECC level M, by version. */
const BLOCKS_M = [
  -1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
];
/** Format bits' ECC-level field for M. */
const ECL_BITS_M = 0;

/** Data + ECC modules a version holds (everything but function patterns), in bits. */
function rawModules(ver: number): number {
  let r = (16 * ver + 128) * ver + 64;
  if (ver >= 2) {
    const n = Math.floor(ver / 7) + 2;
    r -= (25 * n - 10) * n - 55;
    if (ver >= 7) r -= 36;
  }
  return r;
}

/** Data codewords (bytes) a version holds at level M. */
function dataCodewords(ver: number): number {
  return Math.floor(rawModules(ver) / 8) - ECC_PER_BLOCK_M[ver]! * BLOCKS_M[ver]!;
}

/** GF(256) multiply, polynomial 0x11D. */
function gfMul(x: number, y: number): number {
  let z = 0;
  for (let i = 7; i >= 0; i--) {
    z = (z << 1) ^ ((z >>> 7) * 0x11d);
    z ^= ((y >>> i) & 1) * x;
  }
  return z & 0xff;
}

/** The Reed–Solomon generator polynomial of a degree (leading term omitted). */
function rsDivisor(degree: number): number[] {
  const out = new Array<number>(degree).fill(0);
  out[degree - 1] = 1;
  let root = 1;
  for (let i = 0; i < degree; i++) {
    for (let j = 0; j < out.length; j++) {
      const v = gfMul(out[j]!, root);
      out[j] = j + 1 < out.length ? v ^ out[j + 1]! : v;
    }
    root = gfMul(root, 0x02);
  }
  return out;
}

function rsRemainder(data: number[], div: number[]): number[] {
  const out = new Array<number>(div.length).fill(0);
  for (const b of data) {
    const f = b ^ out.shift()!;
    out.push(0);
    div.forEach((c, i) => (out[i] = out[i]! ^ gfMul(c, f)));
  }
  return out;
}

/** The data codewords split into blocks, each with its ECC, interleaved. */
function withEcc(data: number[], ver: number): number[] {
  const nb = BLOCKS_M[ver]!;
  const ecl = ECC_PER_BLOCK_M[ver]!;
  const raw = Math.floor(rawModules(ver) / 8);
  const shortBlocks = nb - (raw % nb);
  const shortLen = Math.floor(raw / nb);
  const div = rsDivisor(ecl);
  const blocks: number[][] = [];
  for (let i = 0, k = 0; i < nb; i++) {
    const dat = data.slice(k, k + shortLen - ecl + (i < shortBlocks ? 0 : 1));
    k += dat.length;
    const ecc = rsRemainder(dat, div);
    if (i < shortBlocks) dat.push(0); // a pad slot, skipped when interleaving
    blocks.push(dat.concat(ecc));
  }
  const out: number[] = [];
  for (let i = 0; i < blocks[0]!.length; i++) {
    blocks.forEach((b, j) => {
      if (i !== shortLen - ecl || j >= shortBlocks) out.push(b[i]!);
    });
  }
  return out;
}

function alignmentPositions(ver: number): number[] {
  if (ver === 1) return [];
  const n = Math.floor(ver / 7) + 2;
  const step = ver === 32 ? 26 : Math.ceil((ver * 4 + 4) / (n * 2 - 2)) * 2;
  const out = [6];
  for (let pos = ver * 4 + 10; out.length < n; pos -= step) out.splice(1, 0, pos);
  return out;
}

/** BCH remainders for the format (15, 5) and version (18, 6) fields. */
function formatBits(mask: number): number {
  const data = (ECL_BITS_M << 3) | mask;
  let rem = data;
  for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  return ((data << 10) | rem) ^ 0x5412;
}

function versionBits(ver: number): number {
  let rem = ver;
  for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
  return (ver << 12) | rem;
}

class Grid {
  readonly ver: number;
  readonly size: number;
  readonly dark: boolean[][];
  readonly fn: boolean[][];
  constructor(ver: number) {
    this.ver = ver;
    this.size = ver * 4 + 17;
    this.dark = Array.from({ length: this.size }, () => new Array<boolean>(this.size).fill(false));
    this.fn = Array.from({ length: this.size }, () => new Array<boolean>(this.size).fill(false));
  }
  set(x: number, y: number, d: boolean) {
    this.dark[y]![x] = d;
    this.fn[y]![x] = true;
  }
  functionPatterns() {
    const s = this.size;
    for (let i = 0; i < s; i++) {
      this.set(6, i, i % 2 === 0);
      this.set(i, 6, i % 2 === 0);
    }
    for (const [cx, cy] of [[3, 3], [s - 4, 3], [3, s - 4]] as const) {
      for (let dy = -4; dy <= 4; dy++) {
        for (let dx = -4; dx <= 4; dx++) {
          const d = Math.max(Math.abs(dx), Math.abs(dy));
          const x = cx + dx;
          const y = cy + dy;
          if (x >= 0 && x < s && y >= 0 && y < s) this.set(x, y, d !== 2 && d !== 4);
        }
      }
    }
    const al = alignmentPositions(this.ver);
    for (let i = 0; i < al.length; i++) {
      for (let j = 0; j < al.length; j++) {
        if ((i === 0 && j === 0) || (i === 0 && j === al.length - 1) || (i === al.length - 1 && j === 0)) continue;
        for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) this.set(al[i]! + dx, al[j]! + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
      }
    }
    this.format(0); // reserved now, written for real after masking
    if (this.ver >= 7) {
      const b = versionBits(this.ver);
      for (let i = 0; i < 18; i++) {
        const d = ((b >>> i) & 1) !== 0;
        const a = s - 11 + (i % 3);
        const c = Math.floor(i / 3);
        this.set(a, c, d);
        this.set(c, a, d);
      }
    }
  }
  format(mask: number) {
    const b = formatBits(mask);
    const bit = (i: number) => ((b >>> i) & 1) !== 0;
    const s = this.size;
    for (let i = 0; i <= 5; i++) this.set(8, i, bit(i));
    this.set(8, 7, bit(6));
    this.set(8, 8, bit(7));
    this.set(7, 8, bit(8));
    for (let i = 9; i < 15; i++) this.set(14 - i, 8, bit(i));
    for (let i = 0; i < 8; i++) this.set(s - 1 - i, 8, bit(i));
    for (let i = 8; i < 15; i++) this.set(8, s - 15 + i, bit(i));
    this.set(8, s - 8, true);
  }
  place(codewords: number[]) {
    const s = this.size;
    let i = 0;
    for (let right = s - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      for (let vert = 0; vert < s; vert++) {
        for (let j = 0; j < 2; j++) {
          const x = right - j;
          const upward = ((right + 1) & 2) === 0;
          const y = upward ? s - 1 - vert : vert;
          if (!this.fn[y]![x] && i < codewords.length * 8) {
            this.dark[y]![x] = ((codewords[i >>> 3]! >>> (7 - (i & 7))) & 1) !== 0;
            i++;
          }
        }
      }
    }
  }
  mask(m: number) {
    for (let y = 0; y < this.size; y++) {
      for (let x = 0; x < this.size; x++) {
        if (this.fn[y]![x]) continue;
        let inv: boolean;
        switch (m) {
          case 0: inv = (x + y) % 2 === 0; break;
          case 1: inv = y % 2 === 0; break;
          case 2: inv = x % 3 === 0; break;
          case 3: inv = (x + y) % 3 === 0; break;
          case 4: inv = (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0; break;
          case 5: inv = ((x * y) % 2) + ((x * y) % 3) === 0; break;
          case 6: inv = (((x * y) % 2) + ((x * y) % 3)) % 2 === 0; break;
          default: inv = (((x + y) % 2) + ((x * y) % 3)) % 2 === 0; break;
        }
        if (inv) this.dark[y]![x] = !this.dark[y]![x];
      }
    }
  }
  /** The standard's penalty score (N1–N4). */
  penalty(): number {
    const s = this.size;
    let p = 0;
    const line = (get: (i: number) => boolean) => {
      let run = 0;
      let prev = false;
      for (let i = 0; i < s; i++) {
        const d = get(i);
        if (i > 0 && d === prev) {
          run++;
        } else {
          if (run >= 5) p += run - 2;
          run = 1;
          prev = d;
        }
      }
      if (run >= 5) p += run - 2;
      // finder-like 1:1:3:1:1 runs with 4 light on either side
      const bits: boolean[] = [];
      for (let i = 0; i < s; i++) bits.push(get(i));
      for (let i = 0; i + 7 <= s; i++) {
        const core = bits[i] && !bits[i + 1] && bits[i + 2] && bits[i + 3] && bits[i + 4] && !bits[i + 5] && bits[i + 6];
        if (!core) continue;
        const lightBefore = [1, 2, 3, 4].every((k) => i - k < 0 || !bits[i - k]);
        const lightAfter = [0, 1, 2, 3].every((k) => i + 7 + k >= s || !bits[i + 7 + k]);
        if (lightBefore || lightAfter) p += 40;
      }
    };
    for (let y = 0; y < s; y++) line((i) => this.dark[y]![i]!);
    for (let x = 0; x < s; x++) line((i) => this.dark[i]![x]!);
    for (let y = 0; y + 1 < s; y++) {
      for (let x = 0; x + 1 < s; x++) {
        const c = this.dark[y]![x];
        if (c === this.dark[y]![x + 1] && c === this.dark[y + 1]![x] && c === this.dark[y + 1]![x + 1]) p += 3;
      }
    }
    let darkCount = 0;
    for (const row of this.dark) for (const d of row) if (d) darkCount++;
    const total = s * s;
    const k = Math.ceil(Math.abs(darkCount * 20 - total * 10) / total) - 1;
    p += Math.max(0, k) * 10;
    return p;
  }
}

/** The QR code of `text` (UTF-8, byte mode, level M) as rows of dark modules. Throws when
 * the text does not fit version 40. */
export function qrEncode(text: string): boolean[][] {
  const bytes = Array.from(new TextEncoder().encode(text));
  let ver = 1;
  for (; ver <= 40; ver++) {
    const countBits = ver < 10 ? 8 : 16;
    if (4 + countBits + bytes.length * 8 <= dataCodewords(ver) * 8) break;
  }
  if (ver > 40) throw new Error("too long for a QR code");
  const cap = dataCodewords(ver) * 8;
  const bits: number[] = [];
  const put = (v: number, n: number) => {
    for (let i = n - 1; i >= 0; i--) bits.push((v >>> i) & 1);
  };
  put(0b0100, 4);
  put(bytes.length, ver < 10 ? 8 : 16);
  for (const b of bytes) put(b, 8);
  put(0, Math.min(4, cap - bits.length));
  put(0, (8 - (bits.length % 8)) % 8);
  for (let pad = 0xec; bits.length < cap; pad ^= 0xec ^ 0x11) put(pad, 8);
  const data: number[] = [];
  for (let i = 0; i < bits.length; i += 8) data.push(bits.slice(i, i + 8).reduce((a, b) => (a << 1) | b, 0));
  const cw = withEcc(data, ver);
  let best: Grid | null = null;
  let bestScore = Infinity;
  for (let m = 0; m < 8; m++) {
    const g = new Grid(ver);
    g.functionPatterns();
    g.place(cw);
    g.mask(m);
    g.format(m);
    const score = g.penalty();
    if (score < bestScore) {
      bestScore = score;
      best = g;
    }
  }
  return best!.dark;
}

/** Draws a code on a canvas, `scale` px a module, with the 4-module quiet zone. */
export function qrDraw(canvas: HTMLCanvasElement, rows: boolean[][], scale = 4) {
  const n = rows.length + 8;
  canvas.width = n * scale;
  canvas.height = n * scale;
  const c = canvas.getContext("2d");
  if (!c) return;
  c.fillStyle = "#ffffff";
  c.fillRect(0, 0, canvas.width, canvas.height);
  c.fillStyle = "#000000";
  rows.forEach((row, y) => row.forEach((d, x) => d && c.fillRect((x + 4) * scale, (y + 4) * scale, scale, scale)));
}
