// WebGL2 NV12 → R'G'B' (SPEC D5): two textures (R8 luma, RG8 chroma at half size),
// BT.709 limited range, chroma upsampled bilinearly. One full-screen triangle strip.
const VS = `#version 300 es
in vec2 p; out vec2 uv;
void main() { uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5); gl_Position = vec4(p, 0.0, 1.0); }`;
const FS = `#version 300 es
precision highp float;
in vec2 uv; out vec4 o;
uniform sampler2D ty; uniform sampler2D tc; uniform float cshift;
void main() {
  float Y = texture(ty, uv).r * 255.0;
  // NV12 chroma is left-cosited (sample k sits on luma column 2k): shift by one luma px.
  vec2 C = texture(tc, uv + vec2(cshift, 0.0)).rg * 255.0;
  float y = (Y - 16.0) / 219.0, cb = (C.x - 128.0) / 224.0, cr = (C.y - 128.0) / 224.0;
  o = vec4(y + 1.5748 * cr, y - 0.187324 * cb - 0.468124 * cr, y + 1.8556 * cb, 1.0);
}`;

export class Nv12Renderer {
  private gl: WebGL2RenderingContext;
  private ty: WebGLTexture;
  private tc: WebGLTexture;
  private w = 0;
  private h = 0;
  private cshift: WebGLUniformLocation | null = null;

  constructor(private canvas: HTMLCanvasElement) {
    const gl = canvas.getContext("webgl2", { antialias: false, alpha: false, depth: false, desynchronized: true, preserveDrawingBuffer: false });
    if (!gl) throw new Error("WebGL2 is not available");
    this.gl = gl;
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? "shader");
      return s;
    };
    const prog = gl.createProgram()!;
    gl.attachShader(prog, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
    gl.useProgram(prog);
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(prog, "p");
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    const tex = (unit: number, name: string) => {
      const t = gl.createTexture()!;
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.uniform1i(gl.getUniformLocation(prog, name), unit);
      return t;
    };
    this.ty = tex(0, "ty");
    this.tc = tex(1, "tc");
    this.cshift = gl.getUniformLocation(prog, "cshift");
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  }

  /** Uploads one frame's planes (tightly packed). */
  upload(w: number, h: number, y: Uint8Array, uv: Uint8Array) {
    const gl = this.gl;
    const cw = w >> 1;
    const ch = h >> 1;
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.ty);
    if (w !== this.w || h !== this.h) gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, w, h, 0, gl.RED, gl.UNSIGNED_BYTE, y);
    else gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RED, gl.UNSIGNED_BYTE, y);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.tc);
    if (w !== this.w || h !== this.h) gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, cw, ch, 0, gl.RG, gl.UNSIGNED_BYTE, uv);
    else gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, cw, ch, gl.RG, gl.UNSIGNED_BYTE, uv);
    if (w !== this.w || h !== this.h) {
      this.w = w;
      this.h = h;
      this.canvas.width = w;
      this.canvas.height = h;
      gl.uniform1f(this.cshift, 1 / w);
    }
  }

  draw() {
    const gl = this.gl;
    gl.viewport(0, 0, this.w, this.h);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }
}
