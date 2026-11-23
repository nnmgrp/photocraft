// Hardware-accelerated Manga SFX fragment & stencil-then-cover shaders (`photocraft-gpu`).
//
// Pure fragment + vertex WGSL (zero compute / storage buffers) so the exact same pipeline runs
// on WebGPU, WebGL2, DX12, Vulkan, and Metal. Every `Rgba32Float` / `R32Float` texture is read
// strictly with `textureLoad` (never `textureSample`) to remain 100% compliant with core WebGPU
// and WebGL2 non-filterable float texture rules.

// =============================================================================
// 1. SHARED UNIFORM, TEXTURE BINDINGS & VERTEX SHADERS
// =============================================================================

struct FxParams {
    // p0: for rasterizer: (doc_x0, doc_y0, inv_doc_w, inv_doc_h)
    //     for pixel FX:   (doc_x0, doc_y0, doc_per_px_x, doc_per_px_y)
    p0: vec4<f32>,
    // p1: (width_px, height_px, param_a, param_b)
    p1: vec4<f32>,
    // p2: (param_c, param_d, param_e, param_f)
    p2: vec4<f32>,
    // p3: (param_g, param_h, param_i, param_j)
    p3: vec4<f32>,
    // c0: primary RGBA color (or gradient start)
    c0: vec4<f32>,
    // c1: secondary RGBA color (or gradient end)
    c1: vec4<f32>,
    // u0: (mode, seed, flags, step_or_count)
    u0: vec4<u32>,
};

@group(0) @binding(0) var<uniform> fx: FxParams;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_c: texture_2d<f32>;

struct QuadOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_quad(@builtin(vertex_index) vid: u32) -> QuadOut {
    // Oversized triangle covering NDC [-1, 1] x [-1, 1].
    let x = f32((vid & 1u) << 2u) - 1.0;
    let y = f32((vid & 2u) << 1u) - 1.0;
    var out: QuadOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Top-left UV (0, 0) -> bottom-right (1, 1).
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@vertex
fn vs_poly(@location(0) doc_pos: vec2<f32>) -> @builtin(position) vec4<f32> {
    let u = (doc_pos.x - fx.p0.x) * fx.p0.z;
    let v = (doc_pos.y - fx.p0.y) * fx.p0.w;
    let ndc_x = u * 2.0 - 1.0;
    let ndc_y = 1.0 - v * 2.0;
    return vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
}

@fragment
fn fs_stencil_noop() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}

@fragment
fn fs_cover_solid() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}

// =============================================================================
// 2. SEG CONTOUR LINE SEEDING FOR GPU JFA (s, d)
// =============================================================================

struct SegVertOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) arc_phase: vec2<f32>,
};

@vertex
fn vs_seg_line(@location(0) doc_pos: vec2<f32>, @location(1) arc_phase: vec2<f32>) -> SegVertOut {
    let u = (doc_pos.x - fx.p0.x) * fx.p0.z;
    let v = (doc_pos.y - fx.p0.y) * fx.p0.w;
    var out: SegVertOut;
    out.pos = vec4<f32>(u * 2.0 - 1.0, 1.0 - v * 2.0, 0.0, 1.0);
    out.arc_phase = arc_phase;
    return out;
}

@fragment
fn fs_jfa_init_seg(inp: SegVertOut) -> @location(0) vec4<f32> {
    let px = floor(inp.pos.xy);
    return vec4<f32>(px.x, px.y, inp.arc_phase.x, inp.arc_phase.y);
}

// =============================================================================
// 3. DETERMINISTIC HASH, VALUE NOISE, FBM & CRISP PAPER GRAIN (1:1 WITH CPU)
// =============================================================================

fn hash_u32(n: u32) -> f32 {
    var x = n ^ 0x9E3779B9u;
    x = (x ^ (x >> 16u)) * 0x85EBCA6Bu;
    x = (x ^ (x >> 13u)) * 0xC2B2AE35u;
    let out = x ^ (x >> 16u);
    return f32(out) / 4294967296.0;
}

fn value_noise_2d(x: f32, y: f32, seed: u32) -> f32 {
    let ix = i32(floor(x));
    let iy = i32(floor(y));
    let fx = x - f32(ix);
    let fy = y - f32(iy);
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);
    let a = hash_u32(u32(ix) * 374761393u + u32(iy) * 668265263u + seed);
    let b = hash_u32(u32(ix + 1) * 374761393u + u32(iy) * 668265263u + seed);
    let c = hash_u32(u32(ix) * 374761393u + u32(iy + 1) * 668265263u + seed);
    let d = hash_u32(u32(ix + 1) * 374761393u + u32(iy + 1) * 668265263u + seed);
    return (a + (b - a) * ux) * (1.0 - uy) + (c + (d - c) * ux) * uy;
}

fn fbm_2d(x: f32, y: f32, seed: u32) -> f32 {
    return value_noise_2d(x, y, seed) * 0.65 + value_noise_2d(x * 2.03 + 17.1, y * 2.03 - 9.4, seed + 101u) * 0.35;
}

fn crisp_paper_grain(x: f32, y: f32, seed: u32) -> f32 {
    let micro = value_noise_2d(x * 1.15, y * 1.15, seed);
    let ix = i32(round(x));
    let iy = i32(round(y));
    let speckle = hash_u32(u32(ix) * 1662606333u + u32(iy) * 84348833u + seed);
    let fiber = value_noise_2d(x * 0.45, y * 0.45, seed + 0x9E37u);
    return clamp(micro * 0.55 + speckle * 0.25 + fiber * 0.20, 0.0, 1.0);
}

// =============================================================================
// 4. ALL 13 PHOTOSHOP + MANGA CHANNEL BLEND MODES (1:1 WITH CPU)
// =============================================================================

fn blend_channel_rgb(b: f32, s: f32, mode: u32) -> f32 {
    switch mode {
        case 1u: { return b * s; } // Multiply
        case 2u: { // ColorBurn
            if s <= 1e-6 { return 0.0; }
            return 1.0 - min((1.0 - b) / s, 1.0);
        }
        case 3u: { return min(b, s); } // Darken
        case 4u: { return 1.0 - (1.0 - b) * (1.0 - s); } // Screen
        case 5u: { // ColorDodge
            if s >= 1.0 - 1e-6 { return 1.0; }
            return min(b / (1.0 - s), 1.0);
        }
        case 6u: { return max(b, s); } // Lighten
        case 7u: { // Overlay
            if b < 0.5 { return 2.0 * b * s; }
            return 1.0 - 2.0 * (1.0 - b) * (1.0 - s);
        }
        case 8u: { // SoftLight
            if s <= 0.5 {
                return b - (1.0 - 2.0 * s) * b * (1.0 - b);
            }
            var d = sqrt(max(b, 0.0));
            if b <= 0.25 {
                d = ((16.0 * b - 12.0) * b + 4.0) * b;
            }
            return b + (2.0 * s - 1.0) * (d - b);
        }
        case 9u: { // HardLight
            if s < 0.5 { return 2.0 * b * s; }
            return 1.0 - 2.0 * (1.0 - b) * (1.0 - s);
        }
        case 10u: { return abs(b - s); } // Difference
        case 11u: { return b + s - 2.0 * b * s; } // Exclusion
        default: { return s; } // Normal (0) or Knockout (12)
    }
}

fn composite_pixel_blend(dst: vec4<f32>, src: vec4<f32>, cov: f32, opacity: f32, mode: u32) -> vec4<f32> {
    let sa = clamp(cov * src.a * opacity, 0.0, 1.0);
    if sa <= 1e-6 {
        return dst;
    }
    if mode == 12u { // Knockout
        let na = dst.a * (1.0 - sa);
        if na <= 1e-6 {
            return vec4<f32>(0.0);
        }
        return vec4<f32>(dst.rgb, na);
    }
    let da = dst.a;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-6 {
        return vec4<f32>(0.0);
    }
    let inv_da = 1.0 - da;
    let inv_sa = 1.0 - sa;
    let sa_da = sa * da;
    let r = clamp((inv_da * sa * src.r + inv_sa * da * dst.r + sa_da * blend_channel_rgb(dst.r, src.r, mode)) / out_a, 0.0, 1.0);
    let g = clamp((inv_da * sa * src.g + inv_sa * da * dst.g + sa_da * blend_channel_rgb(dst.g, src.g, mode)) / out_a, 0.0, 1.0);
    let b = clamp((inv_da * sa * src.b + inv_sa * da * dst.b + sa_da * blend_channel_rgb(dst.b, src.b, mode)) / out_a, 0.0, 1.0);
    return vec4<f32>(r, g, b, out_a);
}

// =============================================================================
// 5. PIXEL FX TEXTURE HELPERS
// =============================================================================

fn active_dims() -> vec2<i32> {
    return max(vec2<i32>(i32(round(fx.p1.x)), i32(round(fx.p1.y))), vec2<i32>(1));
}

fn load_clamped(t: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let d = active_dims();
    let c = clamp(p, vec2<i32>(0), d - vec2<i32>(1));
    return textureLoad(t, c, 0);
}

fn load_zero_oob(t: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let d = active_dims();
    if p.x < 0 || p.y < 0 || p.x >= d.x || p.y >= d.y {
        return vec4<f32>(0.0);
    }
    return textureLoad(t, p, 0);
}

fn sample_bilinear_zero(t: texture_2d<f32>, fp: vec2<f32>) -> vec4<f32> {
    let p0 = vec2<i32>(floor(fp));
    let f = fp - vec2<f32>(p0);
    let v00 = load_zero_oob(t, p0);
    let v10 = load_zero_oob(t, p0 + vec2<i32>(1, 0));
    let v01 = load_zero_oob(t, p0 + vec2<i32>(0, 1));
    let v11 = load_zero_oob(t, p0 + vec2<i32>(1, 1));
    return mix(mix(v00, v10, f.x), mix(v01, v11, f.x), f.y);
}

fn pixel_doc_pos(px: vec2<f32>) -> vec2<f32> {
    return fx.p0.xy + (px + vec2<f32>(0.5)) * fx.p0.zw;
}

// =============================================================================
// 6. MASK UTILITIES & JUMP FLOODING ALGORITHM (JFA)
// =============================================================================

@fragment
fn fs_union_mask(inp: QuadOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(inp.pos.xy));
    let a = load_clamped(tex_a, p).r;
    let b = load_clamped(tex_b, p).r;
    return vec4<f32>(max(a, b), 0.0, 0.0, 1.0);
}

@fragment
fn fs_mul_mask(inp: QuadOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(inp.pos.xy));
    let a = load_clamped(tex_a, p).r;
    let b = load_clamped(tex_b, p).r;
    // u0.x == 1 -> subtract (a * (1 - b)); else multiply (a * b).
    var v = a * b;
    if fx.u0.x == 1u {
        v = clamp(a * (1.0 - b), 0.0, 1.0);
    }
    if fx.u0.z == 1u {
        v = select(0.0, 1.0, v >= 0.5);
    }
    return vec4<f32>(v, 0.0, 0.0, 1.0);
}

@fragment
fn fs_one_bit_face(inp: QuadOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(inp.pos.xy));
    let c = load_clamped(tex_a, p).r;
    let dp = pixel_doc_pos(floor(inp.pos.xy));
    let intensity = fx.p1.z;
    let jitter = (value_noise_2d(dp.x * 0.45, dp.y * 0.45, 701u) - 0.5) * intensity * 0.55;
    let out_c = select(0.0, 1.0, c + jitter >= 0.5);
    return vec4<f32>(out_c, 0.0, 0.0, 1.0);
}

@fragment
fn fs_jfa_init_mask(inp: QuadOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(inp.pos.xy));
    let d = active_dims();
    let m = load_clamped(tex_a, p).r;
    let invert = fx.u0.x; // 0 = seed inside (exterior dist), 1 = seed outside/border (interior SDF)
    var is_seed = false;
    if invert == 0u {
        is_seed = m >= 0.45;
    } else {
        let on_border = p.x == 0 || p.y == 0 || p.x + 1 >= d.x || p.y + 1 >= d.y;
        is_seed = (m < 0.45) || on_border;
    }
    if is_seed {
        return vec4<f32>(f32(p.x), f32(p.y), 0.0, 1.0);
    }
    return vec4<f32>(-1e9, -1e9, 0.0, 0.0);
}

@fragment
fn fs_jfa_step(inp: QuadOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let step = i32(fx.u0.w);
    let d = active_dims();
    var best = load_clamped(tex_a, p);
    var best_d2 = 1e30;
    if best.x > -1e6 {
        let diff = best.xy - pf;
        best_d2 = dot(diff, diff);
    }
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            if dx == 0 && dy == 0 {
                continue;
            }
            let np = p + vec2<i32>(dx * step, dy * step);
            if np.x < 0 || np.y < 0 || np.x >= d.x || np.y >= d.y {
                continue;
            }
            let cand = textureLoad(tex_a, np, 0);
            if cand.x > -1e6 {
                let diff = cand.xy - pf;
                let d2 = dot(diff, diff);
                if d2 < best_d2 {
                    best_d2 = d2;
                    best = cand;
                }
            }
        }
    }
    return best;
}

// =============================================================================
// 7. SEPARABLE BOX BLUR (1-CHANNEL & 2-CHANNEL FOR ETF)
// =============================================================================

@fragment
fn fs_box_blur_1d(inp: QuadOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(inp.pos.xy));
    let rad = i32(fx.u0.w);
    let dir = select(vec2<i32>(0, 1), vec2<i32>(1, 0), fx.u0.x == 0u);
    if rad <= 0 {
        return load_clamped(tex_a, p);
    }
    var sum = vec4<f32>(0.0);
    for (var k: i32 = -rad; k <= rad; k = k + 1) {
        sum = sum + load_clamped(tex_a, p + dir * k);
    }
    return sum / f32(rad * 2 + 1);
}

// =============================================================================
// 8. HALO, STROKE RINGS, DROP SHADOW + 3D EXTRUDE
// =============================================================================

@fragment
fn fs_halo_mask(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = union_cov, tex_b = exterior JFA
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let u = load_clamped(tex_a, p).r;
    let rf = fx.p1.z; // halo radius in render pixels
    var cov = 0.0;
    if u >= 0.45 {
        cov = 1.0;
    } else {
        let seed = load_clamped(tex_b, p);
        if seed.x > -1e6 {
            let dist = length(seed.xy - pf);
            cov = clamp(rf + 0.5 - dist, 0.0, 1.0);
        }
    }
    if fx.u0.z == 1u { // OneBit halo jitter
        let dp = pixel_doc_pos(pf);
        let intensity = fx.p1.w;
        let j = (value_noise_2d(dp.x * 0.3, dp.y * 0.3, 1301u) - 0.5) * intensity * 0.45;
        cov = select(0.0, 1.0, cov + j >= 0.48);
    }
    return vec4<f32>(cov, 0.0, 0.0, 1.0);
}

@fragment
fn fs_one_bit_line_mask(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = face_cov (binarized), tex_b = exterior JFA of face_cov
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let face = load_clamped(tex_a, p).r;
    let rf = fx.p1.z; // line_width * 0.5 in render px
    var dil = face;
    if face < 0.45 {
        let seed = load_clamped(tex_b, p);
        if seed.x > -1e6 {
            dil = clamp(rf + 0.5 - length(seed.xy - pf), 0.0, 1.0);
        }
    }
    let dp = pixel_doc_pos(pf);
    let intensity = fx.p1.w;
    let j = (value_noise_2d(dp.x * 0.38, dp.y * 0.38, 919u) - 0.5) * intensity * 0.5;
    let edge = max(dil - face, 0.0);
    let out_c = select(0.0, 1.0, edge + j >= 0.45);
    return vec4<f32>(out_c, 0.0, 0.0, 1.0);
}

@fragment
fn fs_stroke_ring_mask(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = base mask (union_cov for Outside/Center, face_cov for Inside)
    // tex_b = exterior JFA, tex_c = interior JFA
    // u0.x: 0 = Outside, 1 = Center, 2 = Inside; p1.z = width in render pixels
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let base = load_clamped(tex_a, p).r;
    let width = fx.p1.z;
    let pos_mode = fx.u0.x;
    var ring = 0.0;
    if pos_mode == 0u {
        var dil = 0.0;
        if base >= 0.45 {
            dil = 1.0;
        } else {
            let s = load_clamped(tex_b, p);
            if s.x > -1e6 {
                dil = clamp(width + 0.5 - length(s.xy - pf), 0.0, 1.0);
            }
        }
        ring = clamp(dil - base, 0.0, 1.0);
    } else if pos_mode == 2u {
        let s = load_clamped(tex_c, p);
        let d_in = select(1e6, length(s.xy - pf), s.x > -1e6);
        ring = clamp(width + 0.5 - d_in, 0.0, 1.0) * base;
    } else {
        let half_w = max(width * 0.5, 0.5);
        var dil = 0.0;
        if base >= 0.45 {
            dil = 1.0;
        } else {
            let s = load_clamped(tex_b, p);
            if s.x > -1e6 {
                dil = clamp(half_w + 0.5 - length(s.xy - pf), 0.0, 1.0);
            }
        }
        let s_in = load_clamped(tex_c, p);
        let d_in = select(1e6, length(s_in.xy - pf), s_in.x > -1e6);
        let out_part = clamp(dil - base, 0.0, 1.0);
        let in_part = clamp(half_w + 0.5 - d_in, 0.0, 1.0) * base;
        ring = max(out_part, in_part);
    }
    return vec4<f32>(ring, 0.0, 0.0, 1.0);
}

@fragment
fn fs_shadow_extrude_mask(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = union_cov
    // p1.z = cos_a, p1.w = sin_a, p2.x = extrude_px, p2.y = dist_px, u0.x = mode (0=extrude, 1=shift)
    let pf = floor(inp.pos.xy);
    let dir = vec2<f32>(fx.p1.z, fx.p1.w);
    if fx.u0.x == 0u {
        let depth = fx.p2.x;
        let steps = clamp(i32(ceil(depth)), 2, 48);
        var cov = 0.0;
        for (var st: i32 = 1; st <= 48; st = st + 1) {
            if st > steps {
                break;
            }
            let d = depth * (f32(st) / f32(steps));
            let s = sample_bilinear_zero(tex_a, pf - dir * d).r;
            cov = max(cov, s);
        }
        return vec4<f32>(cov, 0.0, 0.0, 1.0);
    }
    let dist = fx.p2.y;
    let s = sample_bilinear_zero(tex_a, pf - dir * dist).r;
    return vec4<f32>(s, 0.0, 0.0, 1.0);
}

// =============================================================================
// 9. DRY BRUSH ENGINES (A: SDF PHYSICS, B1: SEG JFA, B2: ETF+LIC) & GRAIN
// =============================================================================

@fragment
fn fs_dry_physics(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = base_cov, tex_b = interior JFA
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let base = load_clamped(tex_a, p).r;
    if base <= 0.02 {
        return vec4<f32>(base, 0.0, 0.0, 1.0);
    }
    let inv_bs = select(fx.p3.z, 1.0, fx.p3.z <= 1e-6);
    let s = load_clamped(tex_b, p);
    let sdf_doc = select(0.0, length(s.xy - pf) * fx.p0.z * inv_bs, s.x > -1e6);
    let dp = pixel_doc_pos(pf) * inv_bs;
    let dryness = fx.p1.z;
    let freq = fx.p1.w;
    let warp = fx.p2.x;
    let grain = fx.p2.y;
    let wear = fx.p2.z;
    let sharp = fx.p2.w;
    let cos_a = fx.p3.x;
    let sin_a = fx.p3.y;
    let seed = fx.u0.y;
    let is_bin = fx.u0.z == 1u;

    let u = dp.x * cos_a + dp.y * sin_a;
    let v = -dp.x * sin_a + dp.y * cos_a;
    let w_off = (fbm_2d(u * 0.035, v * 0.035, seed + 11u) - 0.5) * warp;
    let bristle = value_noise_2d(u * 0.025, (v + w_off) * freq, seed);
    let paper = crisp_paper_grain(dp.x, dp.y, seed + 97u);
    let edge_fade = clamp(sdf_doc / (3.0 + wear * 16.0), 0.0, 1.0);
    let ink_score = bristle * (1.0 - grain * 0.35) + paper * (grain * 0.35) + edge_fade * wear * 0.55 - wear * 0.42;
    let thresh = dryness * 0.85;
    var keep = 0.0;
    if is_bin || sharp >= 0.99 {
        keep = select(0.0, 1.0, ink_score >= thresh);
    } else {
        let slope = 5.5 + sharp * sharp * 70.0;
        keep = clamp((ink_score - thresh) * slope + 0.5, 0.0, 1.0);
    }
    return vec4<f32>(base * keep, 0.0, 0.0, 1.0);
}

@fragment
fn fs_dry_seg(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = base_cov, tex_b = Seg JFA (seed_x, seed_y, tangent_angle_rad, s_arc + phase), tex_c = interior JFA
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let base = load_clamped(tex_a, p).r;
    if base <= 0.02 {
        return vec4<f32>(base, 0.0, 0.0, 1.0);
    }
    let inv_bs = select(fx.p3.z, 1.0, fx.p3.z <= 1e-6);
    let seg = load_clamped(tex_b, p);
    let s_in = load_clamped(tex_c, p);
    let sdf_doc = select(0.0, length(s_in.xy - pf) * fx.p0.z * inv_bs, s_in.x > -1e6);
    let dp = pixel_doc_pos(pf) * inv_bs;

    let dryness = fx.p1.z;
    let freq = fx.p1.w;
    let warp = fx.p2.x;
    let grain = fx.p2.y;
    let wear = fx.p2.z;
    let sharp = fx.p2.w;
    let seed = fx.u0.y;
    let is_bin = fx.u0.z == 1u;

    var s_coord = dp.x + dp.y;
    var v_dist = sdf_doc;
    if seg.x > -1e6 {
        // Orthogonal projection onto the winning stem wall's local (tangent, normal) frame:
        // prevents circular distance-field U-turns ("racetrack loops") around sharp corners
        // and lets bristles punch straight through open stem end-caps.
        let t_ang = seg.z;
        let t_vec = vec2<f32>(cos(t_ang), sin(t_ang));
        let n_vec = vec2<f32>(-t_vec.y, t_vec.x);
        let d_doc = (pf - seg.xy) * fx.p0.zw * inv_bs;
        s_coord = seg.w * inv_bs + dot(d_doc, t_vec);
        v_dist = abs(dot(d_doc, n_vec));
    }
    let w_off = (fbm_2d(s_coord * 0.035, v_dist * 0.035, seed + 17u) - 0.5) * warp;
    let bristle = value_noise_2d(s_coord * 0.024, (v_dist + w_off) * freq, seed);
    let paper = crisp_paper_grain(dp.x, dp.y, seed + 113u);
    let edge_fade = clamp(v_dist / (3.0 + wear * 15.0), 0.0, 1.0);
    let ink_score = bristle * (1.0 - grain * 0.35) + paper * (grain * 0.35) + edge_fade * wear * 0.55 - wear * 0.42;
    let thresh = dryness * 0.85;
    var keep = 0.0;
    if is_bin || sharp >= 0.99 {
        keep = select(0.0, 1.0, ink_score >= thresh);
    } else {
        let slope = 5.5 + sharp * sharp * 70.0;
        keep = clamp((ink_score - thresh) * slope + 0.5, 0.0, 1.0);
    }
    return vec4<f32>(base * keep, 0.0, 0.0, 1.0);
}

@fragment
fn fs_etf_seed(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = interior JFA -> outputs tangent (tx, ty, 0, 1)
    let p = vec2<i32>(floor(inp.pos.xy));
    let d_l = load_clamped(tex_a, p + vec2<i32>(-1, 0));
    let d_r = load_clamped(tex_a, p + vec2<i32>(1, 0));
    let d_u = load_clamped(tex_a, p + vec2<i32>(0, -1));
    let d_d = load_clamped(tex_a, p + vec2<i32>(0, 1));
    let sl = select(0.0, length(d_l.xy - vec2<f32>(p + vec2<i32>(-1, 0))), d_l.x > -1e6);
    let sr = select(0.0, length(d_r.xy - vec2<f32>(p + vec2<i32>(1, 0))), d_r.x > -1e6);
    let su = select(0.0, length(d_u.xy - vec2<f32>(p + vec2<i32>(0, -1))), d_u.x > -1e6);
    let sd = select(0.0, length(d_d.xy - vec2<f32>(p + vec2<i32>(0, 1))), d_d.x > -1e6);
    let gx = sr - sl;
    let gy = sd - su;
    let mag = length(vec2<f32>(gx, gy));
    if mag > 1e-4 {
        return vec4<f32>(-gy / mag, gx / mag, 0.0, 1.0);
    }
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_dry_etf_lic(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = base_cov, tex_b = blurred ETF (tx, ty), tex_c = interior JFA
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let base = load_clamped(tex_a, p).r;
    if base <= 0.02 {
        return vec4<f32>(base, 0.0, 0.0, 1.0);
    }
    let inv_bs = select(fx.p3.z, 1.0, fx.p3.z <= 1e-6);
    let step_px = max(fx.p3.w, 1.15);
    let s_in = load_clamped(tex_c, p);
    let sdf_doc = select(0.0, length(s_in.xy - pf) * fx.p0.z * inv_bs, s_in.x > -1e6);
    let dp = pixel_doc_pos(pf) * inv_bs;

    let dryness = fx.p1.z;
    let freq = fx.p1.w;
    let grain = fx.p2.y;
    let wear = fx.p2.z;
    let sharp = fx.p2.w;
    let seed = fx.u0.y;
    let is_bin = fx.u0.z == 1u;
    let steps = i32(clamp(fx.u0.w, 2u, 13u));

    var sum = value_noise_2d(dp.x * freq, dp.y * freq, seed);
    var count = 1.0;
    for (var sgn_i: i32 = 0; sgn_i < 2; sgn_i = sgn_i + 1) {
        let dir_sign = select(1.0, -1.0, sgn_i == 0);
        var fp = pf;
        for (var st: i32 = 0; st < 13; st = st + 1) {
            if st >= steps {
                break;
            }
            let tv = load_clamped(tex_b, vec2<i32>(round(fp))).xy;
            let tl = length(tv);
            let tang = select(vec2<f32>(1.0, 0.0), tv / tl, tl > 1e-5);
            fp = fp + tang * dir_sign * step_px;
            let ndp = pixel_doc_pos(fp) * inv_bs;
            sum = sum + value_noise_2d(ndp.x * freq, ndp.y * freq, seed);
            count = count + 1.0;
        }
    }
    let lic = sum / count;
    let paper = crisp_paper_grain(dp.x, dp.y, seed + 131u);
    let edge_fade = clamp(sdf_doc / (3.0 + wear * 14.0), 0.0, 1.0);
    let ink_score = lic * (1.0 - grain * 0.32) + paper * (grain * 0.32) + edge_fade * wear * 0.55 - wear * 0.42;
    let thresh = dryness * 0.85;
    var keep = 0.0;
    if is_bin || sharp >= 0.99 {
        keep = select(0.0, 1.0, ink_score >= thresh);
    } else {
        let slope = 5.5 + sharp * sharp * 70.0;
        keep = clamp((ink_score - thresh) * slope + 0.5, 0.0, 1.0);
    }
    return vec4<f32>(base * keep, 0.0, 0.0, 1.0);
}

@fragment
fn fs_dry_grain_breakup(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = cov, tex_b = interior JFA (used when u0.x == 1 for Ukiyo kasure)
    let p = vec2<i32>(floor(inp.pos.xy));
    let pf = vec2<f32>(p);
    let cv = load_clamped(tex_a, p).r;
    if cv <= 1e-4 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let dryness = fx.p1.z;
    let sharp = fx.p1.w;
    let seed = fx.u0.y;
    let inv_gs = select(fx.p3.z, 1.0, fx.p3.z <= 1e-6);
    let dp = pixel_doc_pos(pf) * inv_gs;

    if fx.u0.x == 1u {
        // Ukiyo-e Washi kasure (edge-weighted via interior JFA SDF)
        let s_in = load_clamped(tex_b, p);
        let d_in = select(0.0, length(s_in.xy - pf) * fx.p0.z * inv_gs, s_in.x > -1e6);
        let edge_weight = pow(1.0 - clamp((d_in - 1.0) / (2.8 + dryness * 5.0), 0.0, 1.0), 0.85);
        let core_skip = clamp((dryness - 0.55) / 0.45, 0.0, 1.0) * 0.45;
        let local_dry = clamp(dryness * (edge_weight * 1.15 + core_skip), 0.0, 1.0);
        if local_dry <= 0.02 {
            return vec4<f32>(cv, 0.0, 0.0, 1.0);
        }
        let grain = crisp_paper_grain(dp.x, dp.y, seed);
        let streak = value_noise_2d(dp.x * 0.16, dp.y * 0.72, seed + 47u);
        let ink_score = 1.0 - clamp(grain * 0.65 + streak * 0.35, 0.0, 1.0);
        let thresh = local_dry * 0.88;
        var keep = 0.0;
        if sharp >= 0.99 {
            keep = select(0.0, 1.0, ink_score >= thresh);
        } else {
            let slope = 3.0 + sharp * sharp * 55.0;
            keep = clamp((ink_score - thresh) * slope + 0.5, 0.0, 1.0);
        }
        return vec4<f32>(cv * keep, 0.0, 0.0, 1.0);
    }

    // Dry Pencil / Shtack Fill grain breakup
    let up = load_clamped(tex_a, p + vec2<i32>(0, -1)).r;
    let down = load_clamped(tex_a, p + vec2<i32>(0, 1)).r;
    let left = load_clamped(tex_a, p + vec2<i32>(-1, 0)).r;
    let right = load_clamped(tex_a, p + vec2<i32>(1, 0)).r;
    let edge = clamp(cv * min(min(up, down), min(left, right)), 0.0, 1.0);
    let grain = crisp_paper_grain(dp.x, dp.y, seed);
    let streak = value_noise_2d(dp.x * 0.12, dp.y * 0.85, seed + 31u);
    let ink_score = (1.0 - clamp(grain * 0.72 + streak * 0.28, 0.0, 1.0)) + 0.26 * edge;
    let thresh = dryness * 0.92;
    var keep = 0.0;
    if sharp >= 0.99 || fx.u0.z == 1u {
        keep = select(0.0, 1.0, ink_score >= thresh);
    } else {
        let slope = 2.5 + sharp * sharp * 55.0;
        keep = clamp((ink_score - thresh) * slope + 0.5, 0.0, 1.0);
    }
    var out_cov = cv * keep;
    if fx.u0.z == 1u {
        out_cov = select(0.0, 1.0, out_cov >= 0.5);
    }
    return vec4<f32>(out_cov, 0.0, 0.0, 1.0);
}

// =============================================================================
// 10. FILL EVALUATION (SOLID / GRADIENT / GENTONIK SCREENTONE) + 13-MODE COMPOSITE
// =============================================================================

fn eval_screentone_sheet(dp: vec2<f32>) -> vec4<f32> {
    // p2: (dot_size, spacing_x, spacing_y, density)
    // p3: (cos_a, sin_a, line_width, tone_opacity)
    // u0.y = pattern_type (0=dots, 1=lines, 2=crosshatch, 3=noise), u0.z = dot_shape (0=circle, 1=sq, 2=diam, 3=hex)
    let dot_size = max(fx.p2.x, 0.5);
    let sx = max(fx.p2.y, 2.0);
    let sy = max(fx.p2.z, 2.0);
    let density = clamp(fx.p2.w, 0.0, 1.0);
    let cos_a = fx.p3.x;
    let sin_a = fx.p3.y;
    let line_w = max(fx.p3.z, 0.2);
    let tone_op = clamp(fx.p3.w, 0.0, 1.0);
    let pat = fx.u0.y;
    let shape = fx.u0.z;

    let rx = dp.x * cos_a - dp.y * sin_a;
    let ry = dp.x * sin_a + dp.y * cos_a;
    var ink = 0.0;
    if pat == 0u {
        let gx = rx - floor(rx / sx) * sx - sx * 0.5;
        let gy = ry - floor(ry / sy) * sy - sy * 0.5;
        let rad = max(dot_size * 0.5 * sqrt(max(density * 2.0, 0.01)), 0.35);
        var dist = length(vec2<f32>(gx, gy));
        if shape == 1u {
            dist = max(abs(gx), abs(gy));
        } else if shape == 2u {
            dist = (abs(gx) + abs(gy)) * 0.70710678;
        } else if shape == 3u {
            let ax = abs(gx);
            let ay = abs(gy);
            dist = max(ax * 0.8660254 + ay * 0.5, ay);
        }
        ink = clamp(rad + 0.5 - dist, 0.0, 1.0);
    } else if pat == 1u {
        let gy = abs(ry - floor(ry / sy) * sy - sy * 0.5);
        let hw = max(line_w * 0.5 * (0.3 + density * 1.4), 0.3);
        ink = clamp(hw + 0.5 - gy, 0.0, 1.0);
    } else if pat == 2u {
        let gx = abs(rx - floor(rx / sx) * sx - sx * 0.5);
        let gy = abs(ry - floor(ry / sy) * sy - sy * 0.5);
        let hw = max(line_w * 0.5 * (0.3 + density * 1.2), 0.3);
        ink = max(clamp(hw + 0.5 - gx, 0.0, 1.0), clamp(hw + 0.5 - gy, 0.0, 1.0));
    } else {
        let n = crisp_paper_grain(dp.x * (4.0 / sx), dp.y * (4.0 / sy), 4099u);
        ink = select(0.0, 1.0, n < density);
    }
    return vec4<f32>(fx.c0.rgb, fx.c0.a * ink * tone_op);
}

@fragment
fn fs_composite_fill(inp: QuadOut) -> @location(0) vec4<f32> {
    // tex_a = dst_rgba, tex_b = cov_mask
    // u0.x = blend_mode (0..12), u0.w = paint_kind (0=solid, 1=gradient, 2=screentone)
    // p1.z = opacity
    let p = vec2<i32>(floor(inp.pos.xy));
    let dst = load_clamped(tex_a, p);
    let cov = load_clamped(tex_b, p).r;
    if cov <= 1e-5 {
        return dst;
    }
    let paint_kind = fx.u0.w;
    var src = fx.c0;
    if paint_kind == 1u {
        // Gradient: p2 = (full_cx, full_cy, cos_a, sin_a), p3.x = inv_span
        let dp = pixel_doc_pos(floor(inp.pos.xy));
        let rel = dp - fx.p2.xy;
        let t = clamp((rel.x * fx.p2.z + rel.y * fx.p2.w) * fx.p3.x + 0.5, 0.0, 1.0);
        src = mix(fx.c0, fx.c1, t);
    } else if paint_kind == 2u {
        let dp = pixel_doc_pos(floor(inp.pos.xy));
        src = eval_screentone_sheet(dp);
    }
    return composite_pixel_blend(dst, src, cov, fx.p1.z, fx.u0.x);
}

// =============================================================================
// 11. SCREEN BLIT (STRAIGHT RGBA -> PREMULTIPLIED EGUI CANVAS OVERLAY)
// =============================================================================

fn srgb_ch_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn load_premul_zero(tex: texture_2d<f32>, p: vec2<i32>, dims: vec2<i32>, to_lin: bool) -> vec4<f32> {
    if p.x < 0 || p.y < 0 || p.x >= dims.x || p.y >= dims.y {
        return vec4<f32>(0.0);
    }
    let c = textureLoad(tex, p, 0);
    var rgb = c.rgb;
    if to_lin {
        rgb = vec3<f32>(srgb_ch_to_linear(rgb.r), srgb_ch_to_linear(rgb.g), srgb_ch_to_linear(rgb.b));
    }
    return vec4<f32>(rgb * c.a, c.a);
}

@fragment
fn fs_blit_premul(inp: QuadOut) -> @location(0) vec4<f32> {
    let dims_i = max(vec2<i32>(i32(round(fx.p2.y)), i32(round(fx.p2.z))), vec2<i32>(1));
    let dims = vec2<f32>(dims_i);
    var uv = inp.uv;
    var opacity = 1.0;
    if fx.u0.x == 1u {
        let sp = inp.pos.xy;
        uv = vec2<f32>(
            fx.p0.x * sp.x + fx.p0.y * sp.y + fx.p0.z,
            fx.p1.x * sp.x + fx.p1.y * sp.y + fx.p1.z,
        );
        opacity = fx.p2.x;
        if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
            return vec4<f32>(0.0);
        }
    }
    let tc = uv * dims - vec2<f32>(0.5);
    let p0 = vec2<i32>(floor(tc));
    let f = tc - vec2<f32>(p0);
    let to_lin = fx.u0.y == 1u;
    let c00 = load_premul_zero(tex_a, p0, dims_i, to_lin);
    let c10 = load_premul_zero(tex_a, p0 + vec2<i32>(1, 0), dims_i, to_lin);
    let c01 = load_premul_zero(tex_a, p0 + vec2<i32>(0, 1), dims_i, to_lin);
    let c11 = load_premul_zero(tex_a, p0 + vec2<i32>(1, 1), dims_i, to_lin);
    let pm = mix(mix(c00, c10, f.x), mix(c01, c11, f.x), f.y);
    return pm * clamp(opacity, 0.0, 1.0);
}
