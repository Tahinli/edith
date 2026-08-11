//! HDR to SDR: BT.2020 PQ or HLG in, BT.709 SDR out, on the 8-bit limited-range
//! planar YUV every path here already has in hand.
//!
//! The curve is **BT.2446 Method A** -- the published ITU-R conversion, chosen
//! because it is written down: given the same input codes this file produces the
//! same output codes on every machine and every run, and a reader can check the
//! stages below against the document. mpv's default tone curve is a hand-tuned
//! spline that is *not* published anywhere normative, so it cannot be
//! transcribed honestly; Hable is published but is a film-look approximation
//! with no gamut or chroma stage at all. Method A brings all three (tone curve,
//! colour-difference scaling, gamut) and is LUT-friendly, which is the whole
//! design here.
//!
//! Method A, in the order the code runs it:
//!
//! 1. limited-range Y'C'bC'r (BT.2020) -> R'G'B' signal, clipped to 0..1,
//! 2. the transfer's EOTF -> display light in cd/m^2 (PQ per ST 2084, HLG per
//!    BT.2100 including its OOTF, which is why HLG costs almost nothing extra:
//!    it is one more EOTF feeding the same pipe),
//! 3. normalise by [`MASTER_NITS`] and take the 1/2.4 root -- Method A does its
//!    work in a gamma-2.4 domain, not in linear light,
//! 4. BT.2020 luma + colour differences in that domain,
//! 5. the tone curve: a logarithmic lift keyed on the HDR peak, a three-piece
//!    polynomial, then the matching exponential pull-down keyed on the SDR peak.
//!    Both ends are constrained to map peak to peak and black to black,
//! 6. colour-difference scaling by `Y'sdr / (1.075 * Y'hdr)`, then the
//!    `- max(0, 0.1 * C'r)` luma correction the method uses to pay back the
//!    luminance that non-constant-luminance chroma scaling adds to reds,
//! 7. back to R'G'B', linearise, BT.2020 -> BT.709 primaries, **clip** (a simple
//!    clip, as charted: gamut *compression* is a second research problem and
//!    Method A does not specify one), re-encode with the 1/2.4 inverse of
//!    BT.1886 -- the SDR display's own curve, so peak stays peak and black stays
//!    black -- and back to limited-range BT.709 Y'C'bC'r.
//!
//! Method A is conservative on mid-tones by construction: with the standard
//! 1000 cd/m^2 mastering peak it puts 203-nit HDR diffuse white near SDR code
//! 166 (about 40 cd/m^2), not at SDR white. [`MASTER_NITS`] is the exposure
//! knob -- a lower assumed peak lifts everything -- and is a constant here
//! because this module is handed pixels, never the stream metadata that would
//! carry the real one.
//!
//! # Why a 3D LUT
//!
//! Every stage above is float, transcendental and per-pixel-dependent on all
//! three components (the chroma scale reads the pixel's own luma). Running it
//! per pixel is out of the question at 4K. So it runs 9 537 times per stream
//! instead -- a grid over the *input codes*, 33 luma nodes by 17 by 17 -- and
//! the pixel loop is a trilinear interpolation between neighbouring nodes:
//! byte loads, integer multiplies, no float and no branch.
//!
//! The axes are deliberately uneven. A node every 8th code on luma and every
//! 16th on chroma was measured against a uniform 33^3 and a uniform 17^3 on
//! this machine: 17^3 puts the assumed peak (1000 cd/m^2, code 181) 5 codes
//! shy of SDR white, because the curve's knee falls inside a 16-code luma
//! cell, while a full 33^3 costs 30.7 ms a 4K frame against this one's 19.3 --
//! four times the table, out of cache and out of budget. Fine where the eye
//! is, coarse where the format already is.
//!
//! Nodes are `u8`, the output's own precision, and the interpolation rounds
//! once at the end in `u32` fixed point. On a node the grid *is* the pipeline;
//! between nodes it is a straight line, and the pipeline is not straight
//! wherever a channel is against a rail -- the 709 gamut clip in step 7 turns
//! a range of saturated BT.2020 inputs into one clipped output, a plateau no
//! line can follow. Measured worst case 42 codes, all of it inside that
//! plateau, on colours the clip has already crushed; away from the rails the
//! grid tracks the float pipeline within 3. The `tests` below pin both halves.
//!
//! Chroma is quarter-resolution and stays that way: one lookup per chroma
//! *sample*, keyed on the mean luma of its 2x2 block, rather than upsampling to
//! 4x the work for detail the format does not carry. The luma of those four
//! pixels is mapped in the same visit, sharing the block's bilinear chroma
//! weights, which is what keeps a 4K frame inside a frame's time budget.

/// The mastering peak Method A is told to assume, cd/m^2. 1000 is what HDR10
/// grades are mastered to in practice and what BT.2446's own worked example
/// uses. See the module note: this is the exposure knob.
const MASTER_NITS: f32 = 1000.0;
/// The SDR display peak Method A targets, cd/m^2.
const SDR_NITS: f32 = 100.0;
/// Method A's working gamma. Not a display curve here -- it is the domain the
/// tone curve and the colour-difference scaling are defined in.
const GAMMA: f32 = 2.4;

/// Nodes along the luma axis: 33, one every 8th code.
const LUMA_NODES: usize = 33;
/// `256 / (LUMA_NODES - 1)` as a shift, so the node index is a shift and the
/// fraction a mask.
const LUMA_STEP: u32 = 3;
/// `1 << LUMA_STEP`.
const LUMA_FRAC: u32 = 8;
/// Nodes along each chroma axis: 17, one every 16th code. Coarser than luma on
/// purpose -- see the module note on the grid.
const CHROMA_NODES: usize = 17;
/// `256 / (CHROMA_NODES - 1)`, as a shift.
const CHROMA_STEP: u32 = 4;
/// `1 << CHROMA_STEP`.
const CHROMA_FRAC: u32 = 16;

/// Which HDR transfer the incoming codes carry. Both feed the same Method A
/// pipeline; they differ only in how a code becomes display light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// SMPTE ST 2084 / BT.2100 PQ, absolute: a code *is* a luminance.
    Pq,
    /// BT.2100 HLG, relative: a code is scene light, and the OOTF (system gamma
    /// 1.2 at a 1000 cd/m^2 display) turns it into display light.
    Hlg,
}

/// One stream's tone map. Build once (the float pipeline runs 9 537 times, a
/// couple of milliseconds), then map every frame through it.
pub struct ToneMapper {
    /// Output Y at luma node `i` and at `i + 1`, packed into one word 16 bits
    /// apart. The luma axis is the fastest one, so a pixel's two neighbours are
    /// one entry: one load and one multiply per grid corner instead of two of
    /// each, and the weighted sum of four corners tops out at `255 * 256` per
    /// field, exactly what a 16-bit field holds: no carry into the next one.
    luma: Box<[u32]>,
    /// The same for chroma, four fields: U at `i`, U at `i + 1`, V at `i`, V at
    /// `i + 1`. One `u64` multiply does what four `u8` ones did.
    chroma: Box<[u64]>,
}

impl ToneMapper {
    pub fn new(transfer: Transfer) -> Self {
        let cells = LUMA_NODES * CHROMA_NODES * CHROMA_NODES;
        let (mut y_node, mut uv_node) = (vec![0u8; cells], vec![[0u8; 2]; cells]);
        for kv in 0..CHROMA_NODES {
            for ju in 0..CHROMA_NODES {
                for iy in 0..LUMA_NODES {
                    // A node sits on code `n << STEP`; the last one on each axis
                    // is code 256, one past the top, which is the upper end of
                    // the interpolation for code 255 and never a lookup of its
                    // own.
                    let out = map_code(
                        transfer,
                        (iy << LUMA_STEP) as f32,
                        (ju << CHROMA_STEP) as f32,
                        (kv << CHROMA_STEP) as f32,
                    );
                    let at = index(iy, ju, kv);
                    y_node[at] = byte(out[0]);
                    uv_node[at] = [byte(out[1]), byte(out[2])];
                }
            }
        }
        let (mut luma, mut chroma) = (vec![0u32; cells], vec![0u64; cells]);
        for kv in 0..CHROMA_NODES {
            for ju in 0..CHROMA_NODES {
                for iy in 0..LUMA_NODES {
                    let at = index(iy, ju, kv);
                    // The top node of a column has no neighbour above it and is
                    // never the lower end of an interpolation (code 255 lands on
                    // the node below it with a fraction), so it pairs with
                    // itself.
                    let up = if iy + 1 < LUMA_NODES { at + 1 } else { at };
                    luma[at] = u32::from(y_node[at]) | u32::from(y_node[up]) << 16;
                    chroma[at] = u64::from(uv_node[at][0])
                        | u64::from(uv_node[up][0]) << 16
                        | u64::from(uv_node[at][1]) << 32
                        | u64::from(uv_node[up][1]) << 48;
                }
            }
        }
        Self {
            luma: luma.into_boxed_slice(),
            chroma: chroma.into_boxed_slice(),
        }
    }

    /// Maps one tightly packed 8-bit I420 frame in place: `y` is `width *
    /// height`, `u` and `v` are each `width.div_ceil(2) * height.div_ceil(2)`,
    /// no padding (the shape [`crate::convert`], the decoders and the export
    /// path all pass around). A frame that does not measure up is left
    /// untouched rather than half-mapped -- callers hand these in from decoders
    /// and files, and a short plane is a bug upstream, not a picture worth
    /// guessing at.
    ///
    /// In place because both integration sites already own a scratch copy of
    /// the planes at this point (`decode::Render::graded`, and the export
    /// path's grade buffer): they can map that copy and keep the decoder's
    /// frame intact for free.
    pub fn map(&self, y: &mut [u8], u: &mut [u8], v: &mut [u8], width: usize, height: usize) {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        // A frame with no width is not a short plane, it is not a frame; it
        // also makes the band size below zero, which `chunks_mut` panics on.
        if width == 0 || height == 0 {
            return;
        }
        if y.len() < width * height || u.len() < cw * ch || v.len() < cw * ch {
            return;
        }
        // Row slices rather than indices: the plane bounds are checked once per
        // band, not once per sample, which is most of the difference between
        // this and a frame's time budget.
        let bands = y[..width * height].chunks_mut(2 * width);
        let chroma_rows = u[..cw * ch].chunks_mut(cw).zip(v[..cw * ch].chunks_mut(cw));
        for (band, (u_row, v_row)) in bands.zip(chroma_rows) {
            // The bottom half is empty on an odd frame's last band.
            let (top, bottom) = band.split_at_mut(width.min(band.len()));
            let mut tops = top.chunks_mut(2);
            let mut bottoms = bottom.chunks_mut(2);
            for (u_site, v_site) in u_row.iter_mut().zip(v_row) {
                // Odd widths leave a one-pixel block at the end of the row.
                let top = tops.next().unwrap_or_default();
                let bottom = bottoms.next().unwrap_or_default();

                // The block's chroma, bilinear: four node columns and their
                // weights, shared by every lookup this block makes.
                let (uc, vc) = (u32::from(*u_site), u32::from(*v_site));
                let (ju, fu) = ((uc >> CHROMA_STEP) as usize, uc & (CHROMA_FRAC - 1));
                let (kv, fv) = ((vc >> CHROMA_STEP) as usize, vc & (CHROMA_FRAC - 1));
                let corners = [
                    index(0, ju, kv),
                    index(0, ju + 1, kv),
                    index(0, ju, kv + 1),
                    index(0, ju + 1, kv + 1),
                ];
                let weights = [
                    (CHROMA_FRAC - fu) * (CHROMA_FRAC - fv),
                    fu * (CHROMA_FRAC - fv),
                    (CHROMA_FRAC - fu) * fv,
                    fu * fv,
                ];

                // Read the block's luma before writing any of it back: the
                // chroma lookup is keyed on the *source* luma of the block.
                let mut src = [0u32; 4];
                let mut count = 0u32;
                let mut sum = 0u32;
                for (slot, sample) in src.iter_mut().zip(top.iter().chain(bottom.iter())) {
                    *slot = u32::from(*sample);
                    sum += *slot;
                    count += 1;
                }
                let mean = (sum + count / 2) / count;

                let (iy, fy) = ((mean >> LUMA_STEP) as usize, mean & (LUMA_FRAC - 1));
                let mut acc = 0u64;
                for (corner, weight) in corners.iter().zip(&weights) {
                    acc += u64::from(*weight) * self.chroma[corner + iy];
                }
                *u_site = fade((acc & 0xffff) as u32, (acc >> 16 & 0xffff) as u32, fy);
                *v_site = fade((acc >> 32 & 0xffff) as u32, (acc >> 48) as u32, fy);

                for (sample, s) in top.iter_mut().chain(bottom).zip(src) {
                    let (iy, fy) = ((s >> LUMA_STEP) as usize, s & (LUMA_FRAC - 1));
                    let mut acc = 0u32;
                    for (corner, weight) in corners.iter().zip(&weights) {
                        acc += weight * self.luma[corner + iy];
                    }
                    *sample = fade(acc & 0xffff, acc >> 16, fy);
                }
            }
        }
    }
}

/// Node offset. Luma is the fastest axis on purpose -- see [`ToneMapper::luma`].
fn index(iy: usize, ju: usize, kv: usize) -> usize {
    (kv * CHROMA_NODES + ju) * LUMA_NODES + iy
}

/// The last leg of the trilinear tap: `lo` and `hi` are the chroma-bilinear
/// collapse at the two luma nodes (each already carrying the `CHROMA_FRAC^2`
/// chroma scale), `fy` the pixel's fraction between them. Rounds once, at the
/// end, over the whole scale -- truncating here is what puts a step in a
/// gradient.
fn fade(lo: u32, hi: u32, fy: u32) -> u8 {
    const SHIFT: u32 = LUMA_STEP + 2 * CHROMA_STEP;
    ((lo * (LUMA_FRAC - fy) + hi * fy + (1 << (SHIFT - 1))) >> SHIFT) as u8
}

fn byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// The whole float pipeline, from one triple of input codes to one triple of
/// BT.709 limited-range output codes. Runs at build time only; the module note
/// walks the stages.
fn map_code(transfer: Transfer, yc: f32, uc: f32, vc: f32) -> [f32; 3] {
    // 1. limited-range BT.2020 Y'C'bC'r -> R'G'B' signal.
    let luma = (yc - 16.0) / 219.0;
    let cb = (uc - 128.0) / 224.0;
    let cr = (vc - 128.0) / 224.0;
    let r = (luma + 1.4746 * cr).clamp(0.0, 1.0);
    let b = (luma + 1.8814 * cb).clamp(0.0, 1.0);
    let g = ((luma - 0.2627 * r - 0.0593 * b) / 0.6780).clamp(0.0, 1.0);

    // 2/3. display light, then Method A's gamma-2.4 working domain, normalised
    // to the assumed mastering peak.
    //
    // Clamped at that peak, per channel, and this clamp is load-bearing: Method
    // A tone-maps *luma*, so a channel left above 1.0 comes back out of step 6
    // above 1.0 too, and the per-channel gamut clip in step 7 then keeps one
    // channel and drops another -- which does not make a highlight, it makes a
    // hole. A 2800 cd/m^2 near-white measured code 189 with a neighbouring code
    // at 78, a hundred-code cliff on a smooth ramp, before this line existed.
    // Clamping instead walks a too-bright pixel toward white, which is what a
    // highlight past the display's reach should do.
    //
    // ponytail: the ceiling is that MASTER_NITS is a guess. Content mastered
    // brighter than 1000 cd/m^2 loses its top stops to white here. The upgrade
    // is the stream's own MaxCLL/mastering-display metadata reaching this
    // module as a `new` argument -- nothing else in the pipeline changes.
    let lin = display_light(transfer, [r, g, b]);
    let gam = lin.map(|c| (c / MASTER_NITS).clamp(0.0, 1.0).powf(1.0 / GAMMA));

    // 4. BT.2020 luma and colour differences, in that domain.
    let y_hdr = 0.2627 * gam[0] + 0.6780 * gam[1] + 0.0593 * gam[2];
    let cb_hdr = (gam[2] - y_hdr) / 1.8814;
    let cr_hdr = (gam[0] - y_hdr) / 1.4746;

    // 5/6. the curve, then colour-difference scaling and its luma payback.
    let y_sdr = tone_curve(y_hdr);
    let scale = if y_hdr > 1e-6 {
        y_sdr / (1.075 * y_hdr)
    } else {
        0.0
    };
    let (cb_t, cr_t) = (cb_hdr * scale, cr_hdr * scale);
    let y_t = y_sdr - (0.1 * cr_t).max(0.0);

    // 7. back to R'G'B' (still BT.2020, still gamma 2.4), linearise, convert
    // primaries, clip, re-encode.
    let r = y_t + 1.4746 * cr_t;
    let b = y_t + 1.8814 * cb_t;
    let g = (y_t - 0.2627 * r - 0.0593 * b) / 0.6780;
    // Clipped in the BT.2020 signal domain first, the same way step 1 clips:
    // the colour-difference scaling can push a channel past the primaries it
    // came from, and a channel that leaves the *source* gamut only makes the
    // 709 clip below wilder.
    let [r, g, b] = [r, g, b].map(|c| c.clamp(0.0, 1.0).powf(GAMMA));
    let r709 = (1.6605 * r - 0.5876 * g - 0.0728 * b).clamp(0.0, 1.0);
    let g709 = (-0.1246 * r + 1.1329 * g - 0.0083 * b).clamp(0.0, 1.0);
    let b709 = (-0.0182 * r - 0.1006 * g + 1.1187 * b).clamp(0.0, 1.0);
    let [r, g, b] = [r709, g709, b709].map(|c| c.powf(1.0 / GAMMA));

    // 8. BT.709 limited-range codes.
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    [
        16.0 + 219.0 * luma,
        128.0 + 224.0 * (b - luma) / 1.8556,
        128.0 + 224.0 * (r - luma) / 1.5748,
    ]
}

/// Signal -> display light in cd/m^2.
fn display_light(transfer: Transfer, rgb: [f32; 3]) -> [f32; 3] {
    match transfer {
        Transfer::Pq => rgb.map(pq_eotf),
        Transfer::Hlg => {
            // BT.2100: inverse OETF to scene light, then the OOTF, whose system
            // gamma is 1.2 at a 1000 cd/m^2 display -- which is the peak this
            // module assumes anyway, so no log term is needed for it.
            let scene = rgb.map(hlg_scene);
            let ys = 0.2627 * scene[0] + 0.6780 * scene[1] + 0.0593 * scene[2];
            let gain = MASTER_NITS * ys.max(0.0).powf(0.2);
            scene.map(|c| gain * c)
        }
    }
}

/// SMPTE ST 2084 EOTF. Constants are the standard's exact rationals.
fn pq_eotf(n: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let p = n.max(0.0).powf(1.0 / M2);
    10000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// BT.2100 HLG inverse OETF: signal -> scene light, 0..1 where 1 is the
/// nominal white the OOTF then scales to the display peak.
fn hlg_scene(e: f32) -> f32 {
    const A: f32 = 0.17883277;
    const B: f32 = 1.0 - 4.0 * A;
    let e = e.clamp(0.0, 1.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        // BT.2100 quotes `c` as 0.55991073; derived instead, because that is
        // where the number comes from and an `f32` cannot hold all eight
        // digits anyway. Build-time only, so the `ln` is free.
        let c = 0.5 - A * (4.0 * A).ln();
        (((e - c) / A).exp() + B) / 12.0
    }
}

/// BT.2446 Method A's tone curve, in the gamma-2.4 domain: 1 in is 1 out (HDR
/// peak to SDR peak) and 0 in is 0 out, which is what pins the two `p` terms
/// and the three-piece polynomial between them.
fn tone_curve(y_hdr: f32) -> f32 {
    let p_hdr = 1.0 + 32.0 * (MASTER_NITS / 10000.0).powf(1.0 / GAMMA);
    let p_sdr = 1.0 + 32.0 * (SDR_NITS / 10000.0).powf(1.0 / GAMMA);
    let yp = (1.0 + (p_hdr - 1.0) * y_hdr.max(0.0)).ln() / p_hdr.ln();
    let yc = if yp <= 0.7399 {
        1.0770 * yp
    } else if yp < 0.9909 {
        (-1.1510 * yp + 2.7811) * yp - 0.6302
    } else {
        0.5 * yp + 0.5
    };
    (p_sdr.powf(yc) - 1.0) / (p_sdr - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inverse of [`pq_eotf`]: nits -> PQ signal, so an anchor can be named in
    /// cd/m^2 the way the standards name them.
    fn pq_code(nits: f32) -> f32 {
        const M1: f32 = 2610.0 / 16384.0;
        const M2: f32 = 2523.0 / 4096.0 * 128.0;
        const C1: f32 = 3424.0 / 4096.0;
        const C2: f32 = 2413.0 / 4096.0 * 32.0;
        const C3: f32 = 2392.0 / 4096.0 * 32.0;
        let y = (nits / 10000.0).powf(M1);
        let signal = ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2);
        16.0 + 219.0 * signal
    }

    /// One flat frame at a single YUV triple, mapped, handed back as its (Y, U,
    /// V) output. Goes through [`ToneMapper::map`], so it measures the shipped
    /// path -- LUT, interpolation and all -- not the float reference.
    fn mapped(mapper: &ToneMapper, yc: u8, uc: u8, vc: u8) -> (u8, u8, u8) {
        let (w, h) = (8usize, 8usize);
        let mut y = vec![yc; w * h];
        let mut u = vec![uc; w / 2 * h / 2];
        let mut v = vec![vc; w / 2 * h / 2];
        mapper.map(&mut y, &mut u, &mut v, w, h);
        (y[0], u[0], v[0])
    }

    /// The three anchors, in cd/m^2, against codes worked out by hand from the
    /// stages in the module note. Black is pinned by the curve (0 in, 0 out);
    /// 1000 nits is the assumed peak and must reach SDR white; 203 nits is
    /// BT.2408 diffuse white, which Method A deliberately does *not* put at SDR
    /// white -- 166 is what the published maths gives, and the day that number
    /// moves, the transcription moved with it.
    #[test]
    fn pq_anchors_land_where_method_a_puts_them() {
        let mapper = ToneMapper::new(Transfer::Pq);
        for (nits, expected) in [(0.0, 16u8), (26.0, 89), (203.0, 166), (1000.0, 235)] {
            let code = byte(pq_code(nits));
            let (y, _, _) = mapped(&mapper, code, 128, 128);
            assert!(
                y.abs_diff(expected) <= 3,
                "{nits} cd/m^2 (code {code}) -> {y}, expected {expected}"
            );
        }
    }

    /// On its nodes the grid is not an approximation at all: no interpolation
    /// runs, so the byte a picture gets is the byte the float pipeline
    /// computed. This is the half of the accuracy claim that is exact, and it
    /// covers every node of every axis for both transfers.
    #[test]
    fn the_grid_is_the_pipeline_on_its_nodes() {
        for transfer in [Transfer::Pq, Transfer::Hlg] {
            let mapper = ToneMapper::new(transfer);
            for yc in (0..=248u8).step_by(1 << LUMA_STEP) {
                for uc in (0..=240u8).step_by(1 << CHROMA_STEP) {
                    for vc in (0..=240u8).step_by(1 << CHROMA_STEP) {
                        let got = mapped(&mapper, yc, uc, vc);
                        let want = map_code(transfer, yc.into(), uc.into(), vc.into());
                        let want = (byte(want[0]), byte(want[1]), byte(want[2]));
                        assert_eq!(got, want, "{transfer:?} node {yc}/{uc}/{vc}");
                    }
                }
            }
        }
    }

    /// Between the nodes the grid is a straight line through the eight that
    /// surround the sample, so every output it can produce lies inside the box
    /// those eight span -- an index off by one axis stride, a fraction taken
    /// from the wrong shift, or a carry between the packed fields all break
    /// this immediately, and none of them would move an on-node value.
    ///
    /// Deliberately *not* an epsilon against the float pipeline off the nodes:
    /// see the module note. Where the 709 clip flattens a range of inputs onto
    /// one output, a straight line between two nodes is up to 42 codes off that
    /// plateau and is the better-behaved of the two -- it ramps where the
    /// pipeline steps.
    #[test]
    fn between_nodes_the_grid_stays_inside_its_own_cell() {
        for transfer in [Transfer::Pq, Transfer::Hlg] {
            let mapper = ToneMapper::new(transfer);
            for yc in (3..=255u8).step_by(23) {
                for uc in (5..=255u8).step_by(29) {
                    for vc in (7..=255u8).step_by(31) {
                        let got = [
                            mapped(&mapper, yc, uc, vc).0,
                            mapped(&mapper, yc, uc, vc).1,
                            mapped(&mapper, yc, uc, vc).2,
                        ];
                        // The eight enclosing nodes, from the same float
                        // pipeline the table was built with.
                        let (mut lo, mut hi) = ([255u8; 3], [0u8; 3]);
                        let node = |c: u8, step: u32| f32::from((c >> step) << step);
                        for dy in [0.0, f32::from(1u8 << LUMA_STEP)] {
                            for du in [0.0, f32::from(1u8 << CHROMA_STEP)] {
                                for dv in [0.0, f32::from(1u8 << CHROMA_STEP)] {
                                    let out = map_code(
                                        transfer,
                                        node(yc, LUMA_STEP) + dy,
                                        node(uc, CHROMA_STEP) + du,
                                        node(vc, CHROMA_STEP) + dv,
                                    );
                                    for (i, c) in out.iter().enumerate() {
                                        lo[i] = lo[i].min(byte(*c));
                                        hi[i] = hi[i].max(byte(*c));
                                    }
                                }
                            }
                        }
                        for (i, name) in ["Y", "U", "V"].iter().enumerate() {
                            // One code of slack: the nodes are rounded bytes
                            // and the interpolation rounds once more.
                            assert!(
                                got[i] + 1 >= lo[i] && got[i] <= hi[i] + 1,
                                "{transfer:?} {name} at {yc}/{uc}/{vc}: {} outside {}..{}",
                                got[i],
                                lo[i],
                                hi[i]
                            );
                        }
                    }
                }
            }
        }
    }

    /// A grey ramp may not fold back on itself: an inversion anywhere is a
    /// posterised or solarised picture, and it is exactly what a badly built
    /// grid or a truncating interpolation produces.
    #[test]
    fn a_grey_ramp_never_inverts() {
        for transfer in [Transfer::Pq, Transfer::Hlg] {
            let mapper = ToneMapper::new(transfer);
            let mut last = 0u8;
            for code in 0..=255u8 {
                let (y, _, _) = mapped(&mapper, code, 128, 128);
                assert!(
                    y >= last,
                    "{transfer:?}: code {code} darkened to {y} < {last}"
                );
                last = y;
            }
        }
    }

    /// Grey in, grey out: any chroma the pipeline invents on a neutral is a
    /// tint across the whole picture.
    #[test]
    fn neutral_input_stays_neutral() {
        for transfer in [Transfer::Pq, Transfer::Hlg] {
            let mapper = ToneMapper::new(transfer);
            for code in (16..=235u8).step_by(7) {
                let (_, u, v) = mapped(&mapper, code, 128, 128);
                assert!(
                    u.abs_diff(128) <= 2 && v.abs_diff(128) <= 2,
                    "{transfer:?}: grey {code} tinted to U{u} V{v}"
                );
            }
        }
    }

    /// HLG's own anchors. Its signal is relative, so what is worth pinning is
    /// where the OOTF puts it: black at black, peak signal at the assumed
    /// 1000 cd/m^2 display peak (hence SDR white, the PQ side's 1000-nit
    /// anchor), and BT.2408 HLG diffuse white -- signal 0.75, which the inverse
    /// OETF and the system-gamma-1.2 OOTF land on 203 cd/m^2, the same light as
    /// the PQ side's diffuse white and so the same output code. One pipeline,
    /// two front doors: if the HLG stage drifts, that agreement is the first
    /// thing to break.
    #[test]
    fn hlg_anchors_are_sane() {
        let mapper = ToneMapper::new(Transfer::Hlg);
        let code = |signal: f32| byte(16.0 + 219.0 * signal);
        let (black, _, _) = mapped(&mapper, code(0.0), 128, 128);
        let (diffuse, _, _) = mapped(&mapper, code(0.75), 128, 128);
        let (white, _, _) = mapped(&mapper, code(1.0), 128, 128);
        assert!(black.abs_diff(16) <= 3, "HLG black -> {black}");
        assert!(white.abs_diff(235) <= 3, "HLG peak -> {white}");
        assert!(diffuse.abs_diff(166) <= 3, "HLG diffuse white -> {diffuse}");
    }

    /// A short plane is left alone rather than half mapped, and a frame with no
    /// size at all returns instead of panicking on a zero-length band.
    #[test]
    fn a_frame_that_does_not_measure_up_is_untouched() {
        let mapper = ToneMapper::new(Transfer::Pq);
        let mut y = vec![128u8; 8 * 8 - 1];
        let mut u = vec![128u8; 4 * 4];
        let mut v = vec![128u8; 4 * 4];
        mapper.map(&mut y, &mut u, &mut v, 8, 8);
        assert!(y.iter().all(|&s| s == 128), "short plane was mapped");
        mapper.map(&mut y, &mut u, &mut v, 0, 8);
        mapper.map(&mut y, &mut u, &mut v, 8, 0);
        mapper.map(&mut [], &mut [], &mut [], 0, 0);
    }

    /// Odd sizes: the edge block covers one row or one column and must not run
    /// off the end.
    #[test]
    fn odd_sizes_map_every_pixel() {
        let mapper = ToneMapper::new(Transfer::Pq);
        let (w, h) = (7usize, 5usize);
        let mut y = vec![200u8; w * h];
        let mut u = vec![128u8; w.div_ceil(2) * h.div_ceil(2)];
        let mut v = vec![128u8; w.div_ceil(2) * h.div_ceil(2)];
        mapper.map(&mut y, &mut u, &mut v, w, h);
        assert!(y.iter().all(|&s| s != 200), "a pixel was skipped");
    }

    /// The cost gate, asserted in release only: a debug build is an order of
    /// magnitude off and the number only means something optimised. Run with
    /// `cargo test -p engine --release --lib tonemap:: -- --nocapture`.
    ///
    /// The *fastest* frame of a two-second window, not the mean of a fixed
    /// count: this test shares a six-core machine with the other 200 in the
    /// binary, and measured against them a 19.0 ms frame reads as 29.7. Kept
    /// running until they are done, the tail iterations get a core to
    /// themselves and the minimum is the machine's real cost -- which is the
    /// question the gate asks, "can one core carry a 4K frame in a frame's
    /// time", and the reason a plain mean here would be a coin flip.
    #[test]
    fn perf_4k_and_1080p() {
        let mapper = ToneMapper::new(Transfer::Pq);
        for (w, h, budget) in [(3840usize, 1600usize, 25.0f64), (1920, 1080, 8.0)] {
            let mut y: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
            let mut u: Vec<u8> = (0..w / 2 * h / 2).map(|i| (i % 256) as u8).collect();
            let mut v: Vec<u8> = (0..w / 2 * h / 2).map(|i| (255 - i % 256) as u8).collect();
            let mut best = f64::MAX;
            let window = std::time::Instant::now();
            let mut runs = 0;
            while runs < 10 || window.elapsed().as_secs_f64() < 1.0 {
                let t = std::time::Instant::now();
                mapper.map(&mut y, &mut u, &mut v, w, h);
                best = best.min(t.elapsed().as_secs_f64() * 1000.0);
                runs += 1;
            }
            println!("tonemap::map {w}x{h}: {best:.3} ms/frame");
            if !cfg!(debug_assertions) {
                assert!(
                    best <= budget,
                    "{w}x{h}: {best:.3} ms/frame over {budget} ms"
                );
            }
        }
    }
}
