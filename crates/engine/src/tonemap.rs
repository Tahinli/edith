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
//! 3. normalise by the assumed mastering peak ([`Preset`]) and take the 1/2.4
//!    root -- Method A does its
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
//! Method A is conservative on mid-tones by construction: told the standard
//! 1000 cd/m^2 mastering peak it puts 203-nit HDR diffuse white near SDR code
//! 166 (about 40 cd/m^2), not at SDR white -- a picture a viewer reads as dark.
//! The assumed peak is the exposure knob that answers it -- a lower one lifts
//! everything -- and *which* answer a project wants is a matter of taste, not of
//! correctness: a reference rendition and a vivid one are both honest pictures
//! of the same film. So it is a [`Preset`] the viewer picks rather than a
//! constant -- read, on the faithful one, against the peak the film itself
//! declares (`declared_peak` at [`ToneMapper::new`], the demuxer's MaxCLL or
//! mastering-display maximum). The picked rendition still decides; the metadata
//! only says what "the peak the content really has" is for this film.
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
//! this machine, back when the assumed peak was 1000 cd/m^2: 17^3 put that
//! peak's own code 5 codes shy of SDR white, because the curve's knee falls
//! inside a 16-code luma cell, while a full 33^3 costs 30.7 ms a 4K frame
//! against this one's 19.3 -- four times the table, out of cache and out of
//! budget. Fine where the eye is, coarse where the format already is.
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

/// Which rendition of the same conversion a project is watching: two numbers,
/// the assumed mastering peak and a chroma gain, and *only* those two -- every
/// preset runs the very same BT.2446 Method A stages, so the hue relationships
/// the method is chosen for hold whichever one is picked.
///
/// The peak is Method A's exposure knob (see the module note): told the 1000
/// cd/m^2 an HDR10 grade is really mastered to, the published conversion lands
/// 203-nit diffuse white on code 166 -- broadcast-faithful, and darker than a
/// player looks; told a lower one it lifts the whole picture. The gain is
/// applied *after* the map, on the SDR colour differences, so it enriches the
/// picture a viewer is shown rather than bending a stage of the standard.
///
/// # How a film's own metadata composes with the pick
///
/// Only [`Reference`](Preset::Reference) reads it, and that is the rule rather
/// than an omission: it is the rendition that promises *the mastering peak the
/// content really has*, so a film that declares 1759 cd/m^2 is converted at
/// 1759 and its 1000 is a fallback for a file that declared nothing.
/// [`Standard`](Preset::Standard) and [`Vivid`](Preset::Vivid) name a fixed
/// exposure -- 400 and 250 -- which is the whole of what a viewer picks them
/// for; letting a film's metadata move those numbers would make the same pick
/// mean a different brightness on every file, which is the opposite of what
/// picking one is for. So: metadata is read *against* the picked preset, never
/// in place of it, and two of the three are deliberately deaf to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    /// The published conversion, at the mastering peak the content really has
    /// -- the film's own declared peak where it has one (see the note above).
    /// The default: what the document says, and what a second reference can be
    /// checked against.
    #[default]
    Reference,
    /// Display-referred: the peak lowered to 400 so diffuse white lands near SDR
    /// white and the picture reads as bright as a player shows it.
    Standard,
    /// Brighter again, and slightly richer -- the colour differences scaled up a
    /// tenth after the map.
    Vivid,
}

impl Preset {
    /// Every preset, in the order a list offers them: reference first, brightest
    /// last.
    pub const ALL: [Preset; 3] = [Preset::Reference, Preset::Standard, Preset::Vivid];

    /// The mastering peak Method A is told to assume, cd/m^2: this rendition's
    /// own number, before a file gets a say ([`Self::master_nits_for`]).
    pub fn master_nits(self) -> f32 {
        match self {
            Preset::Reference => 1000.0,
            Preset::Standard => 400.0,
            Preset::Vivid => 250.0,
        }
    }

    /// The peak Method A is really run at, given what the file declared
    /// ([`crate::demux::Demuxer::light`] -> [`crate::colorspace::ContentLight::peak`]).
    /// The composition rule is on the enum: the reference rendition takes the
    /// film's number, the other two keep their own.
    ///
    /// A declared peak is a stranger's number, so it is checked before it
    /// becomes the divisor of every pixel: below SDR white the whole picture
    /// clips to white, and a NaN would poison the entire table into black.
    /// Out of range -- or a NaN, which no comparison admits -- is treated as
    /// not declared. The ceiling is PQ's own 10 000 cd/m^2.
    pub fn master_nits_for(self, declared_peak: Option<f32>) -> f32 {
        match (self, declared_peak) {
            (Preset::Reference, Some(peak)) if (SDR_NITS..=10_000.0).contains(&peak) => peak,
            _ => self.master_nits(),
        }
    }

    /// What the SDR colour differences are multiplied by after the map. `1.0`
    /// leaves the method's own output untouched, byte for byte.
    pub fn chroma_gain(self) -> f32 {
        match self {
            Preset::Reference | Preset::Standard => 1.0,
            Preset::Vivid => 1.10,
        }
    }

    /// The name a project file carries ([`crate::edith`]) -- lower case, one
    /// word, and the inverse of [`from_name`](Preset::from_name).
    pub fn name(self) -> &'static str {
        match self {
            Preset::Reference => "reference",
            Preset::Standard => "standard",
            Preset::Vivid => "vivid",
        }
    }

    /// Reads one back. `None` for anything else, which a project file refuses by
    /// name rather than silently rendering another way.
    pub fn from_name(name: &[u8]) -> Option<Self> {
        Preset::ALL.into_iter().find(|p| p.name().as_bytes() == name)
    }
}

/// Which HDR transfer the incoming codes carry. Both feed the same Method A
/// pipeline; they differ only in how a code becomes display light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// SMPTE ST 2084 / BT.2100 PQ, absolute: a code *is* a luminance.
    Pq,
    /// BT.2100 HLG, relative: a code is scene light, and the OOTF (system gamma
    /// 1.2 at a 1000 cd/m^2 display, less at a dimmer one) turns it into
    /// display light.
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
    /// The peak this table was really built at, cd/m^2 -- the picked preset's
    /// read against what the file declared ([`Preset::master_nits_for`]). Kept
    /// so a caller can say which number a picture came out of.
    peak: f32,
}

impl ToneMapper {
    /// `declared_peak` is the film's own peak brightness where it has one
    /// ([`crate::demux::Demuxer::light`]), [`None`] for a file that declared
    /// nothing. Which renditions act on it is [`Preset`]'s composition rule.
    pub fn new(transfer: Transfer, preset: Preset, declared_peak: Option<f32>) -> Self {
        let peak = preset.master_nits_for(declared_peak);
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
                        preset,
                        peak,
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
            peak,
        }
    }

    /// The mastering peak this table was built at, cd/m^2: the film's own where
    /// the rendition reads it, the rendition's own otherwise. What a test -- or
    /// a caller wanting to say which number a picture came out of -- asks.
    pub fn peak(&self) -> f32 {
        self.peak
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
fn map_code(
    transfer: Transfer,
    preset: Preset,
    master_nits: f32,
    yc: f32,
    uc: f32,
    vc: f32,
) -> [f32; 3] {
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
    // The reference rendition is now told the film's own declared peak
    // ([`Preset::master_nits_for`]), so what walks to white here is what the
    // film says it never masters above; the two fixed renditions keep their
    // exposure and still clamp an HDR10 grade's top end into white on purpose.
    let lin = display_light(transfer, master_nits, [r, g, b]);
    let gam = lin.map(|c| (c / master_nits).clamp(0.0, 1.0).powf(1.0 / GAMMA));

    // 4. BT.2020 luma and colour differences, in that domain.
    let y_hdr = 0.2627 * gam[0] + 0.6780 * gam[1] + 0.0593 * gam[2];
    let cb_hdr = (gam[2] - y_hdr) / 1.8814;
    let cr_hdr = (gam[0] - y_hdr) / 1.4746;

    // 5/6. the curve, then colour-difference scaling and its luma payback.
    let y_sdr = tone_curve(y_hdr, master_nits);
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

    // 8. BT.709 limited-range codes, the preset's chroma gain on the two colour
    // differences.
    //
    // A *scale* about 128 and never a shift, applied here and nowhere earlier:
    // 128 is neutral, so a grey stays exactly grey at any gain (`128 - 128` is
    // zero however it is multiplied) and only a pixel that already had colour
    // gets more of it. Clamped to the legal 16..240 chroma range, which at gain
    // 1.0 is a clamp the maths above cannot reach -- the untouched presets come
    // out byte for byte what Method A produced.
    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let gain = preset.chroma_gain();
    [
        16.0 + 219.0 * luma,
        (128.0 + gain * 224.0 * (b - luma) / 1.8556).clamp(16.0, 240.0),
        (128.0 + gain * 224.0 * (r - luma) / 1.5748).clamp(16.0, 240.0),
    ]
}

/// Signal -> display light in cd/m^2.
fn display_light(transfer: Transfer, master_nits: f32, rgb: [f32; 3]) -> [f32; 3] {
    match transfer {
        Transfer::Pq => rgb.map(pq_eotf),
        Transfer::Hlg => {
            // BT.2100: inverse OETF to scene light, then the OOTF, whose system
            // gamma is 1.2 at a 1000 cd/m^2 display and follows the display
            // peak from there by the standard's own log term -- so the peak in
            // force, the picked rendition's read against the film's declared
            // one, is the peak the OOTF is built for rather than a number the
            // choice or the metadata silently invalidates.
            let scene = rgb.map(hlg_scene);
            let ys = 0.2627 * scene[0] + 0.6780 * scene[1] + 0.0593 * scene[2];
            let gamma = 1.2 + 0.42 * (master_nits / 1000.0).log10();
            // Black is black, said outright rather than left to the arithmetic:
            // below a 1000 cd/m^2 peak the system gamma is *under* 1, so the
            // `ys^(gamma - 1)` term diverges as the scene goes black and the
            // `0 * inf` that follows is a NaN -- which byte-casts to 0 and puts
            // a hole where black belongs (measured: HLG black rendered code 0
            // under the vivid preset before this line). Kept above 1000 too,
            // where a declared peak puts the gamma over 1 and the term is
            // merely 0: the guard costs one comparison at build time.
            let ys = ys.max(0.0);
            let gain = match ys > 0.0 {
                true => master_nits * ys.powf(gamma - 1.0),
                false => 0.0,
            };
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
fn tone_curve(y_hdr: f32, master_nits: f32) -> f32 {
    let p_hdr = 1.0 + 32.0 * (master_nits / 10000.0).powf(1.0 / GAMMA);
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

    /// The anchors, in cd/m^2, against the codes the stages in the module note
    /// give for each preset's assumed peak. Black is pinned by the curve (0 in,
    /// 0 out); the assumed peak itself must reach SDR white; 203 nits is BT.2408
    /// diffuse white, and *where it lands is the whole difference between the
    /// presets* -- 166 at the published 1000 cd/m^2 (broadcast-faithful, and the
    /// reason the reference rendition reads dark), 205 at 400, 226 at 250. The
    /// day one of these moves, the transcription moved with it.
    ///
    /// The last row of each says what happens above the assumed peak, asserted
    /// rather than described: it is white.
    #[test]
    fn pq_anchors_land_where_method_a_puts_them() {
        for (preset, diffuse) in [
            (Preset::Reference, 166u8),
            (Preset::Standard, 205),
            (Preset::Vivid, 226),
        ] {
            let peak = preset.master_nits();
            let mapper = ToneMapper::new(Transfer::Pq, preset, None);
            for (nits, expected) in [(0.0, 16u8), (203.0, diffuse), (peak, 235), (10000.0, 235)] {
                let code = byte(pq_code(nits));
                let (y, _, _) = mapped(&mapper, code, 128, 128);
                assert!(
                    y.abs_diff(expected) <= 3,
                    "{preset:?}: {nits} cd/m^2 (code {code}) -> {y}, expected {expected}"
                );
            }
        }
    }

    /// What the presets are *for*, on one code: brighter is brighter, and the
    /// vivid one has more colour in it. A preset that renamed the same numbers
    /// would pass every other test in this module and fail this one.
    ///
    /// The chroma reading is the distance from neutral, which is the thing the
    /// gain scales; the grey beside it is the neutrality that must survive it,
    /// so "richer" cannot have been bought with a tint.
    #[test]
    fn a_brighter_preset_is_brighter_and_a_vivid_one_is_richer() {
        // BT.2408 diffuse white, and a saturated BT.2020 red beside it.
        let (diffuse, red) = (byte(pq_code(203.0)), (100u8, 90u8, 200u8));
        let luma = |preset| {
            mapped(
                &ToneMapper::new(Transfer::Pq, preset, None),
                diffuse,
                128,
                128,
            )
            .0
        };
        let (reference, standard, vivid) = (
            luma(Preset::Reference),
            luma(Preset::Standard),
            luma(Preset::Vivid),
        );
        assert!(
            reference < standard && standard < vivid,
            "reference {reference}, standard {standard}, vivid {vivid}"
        );
        let colour = |preset| {
            let (_, u, v) = mapped(
                &ToneMapper::new(Transfer::Pq, preset, None),
                red.0,
                red.1,
                red.2,
            );
            (u.abs_diff(128), v.abs_diff(128))
        };
        let (flat, rich) = (colour(Preset::Standard), colour(Preset::Vivid));
        // Both differences read together, and neither of them allowed to shrink:
        // this red is saturated enough that its V is already against the legal
        // 240 rail under both presets, so the gain has nowhere to take *that*
        // one -- which is the clamp doing its job, not the gain failing to.
        assert!(
            rich.0 + rich.1 > flat.0 + flat.1 && rich.0 >= flat.0 && rich.1 >= flat.1,
            "vivid's red {rich:?} is no further from neutral than standard's {flat:?}"
        );
    }

    /// On its nodes the grid is not an approximation at all: no interpolation
    /// runs, so the byte a picture gets is the byte the float pipeline
    /// computed. This is the half of the accuracy claim that is exact, and it
    /// covers every node of every axis for both transfers.
    #[test]
    fn the_grid_is_the_pipeline_on_its_nodes() {
        for preset in Preset::ALL {
            for transfer in [Transfer::Pq, Transfer::Hlg] {
                let mapper = ToneMapper::new(transfer, preset, None);
                for yc in (0..=248u8).step_by(1 << LUMA_STEP) {
                    for uc in (0..=240u8).step_by(1 << CHROMA_STEP) {
                        for vc in (0..=240u8).step_by(1 << CHROMA_STEP) {
                            let got = mapped(&mapper, yc, uc, vc);
                            let want = map_code(
                                transfer,
                                preset,
                                mapper.peak(),
                                yc.into(),
                                uc.into(),
                                vc.into(),
                            );
                            let want = (byte(want[0]), byte(want[1]), byte(want[2]));
                            assert_eq!(got, want, "{preset:?} {transfer:?} node {yc}/{uc}/{vc}");
                        }
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
        // Every preset, since the vivid one's chroma gain is a stage the nodes
        // carry and the interpolation between them must not exaggerate.
        for (transfer, preset) in [Transfer::Pq, Transfer::Hlg]
            .into_iter()
            .flat_map(|t| Preset::ALL.map(|p| (t, p)))
        {
            let mapper = ToneMapper::new(transfer, preset, None);
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
                                        preset,
                                        mapper.peak(),
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
                                "{preset:?} {transfer:?} {name} at {yc}/{uc}/{vc}: {} outside {}..{}",
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
        for (transfer, preset) in [Transfer::Pq, Transfer::Hlg]
            .into_iter()
            .flat_map(|t| Preset::ALL.map(|p| (t, p)))
        {
            let mapper = ToneMapper::new(transfer, preset, None);
            let mut last = 0u8;
            for code in 0..=255u8 {
                let (y, _, _) = mapped(&mapper, code, 128, 128);
                assert!(
                    y >= last,
                    "{preset:?} {transfer:?}: code {code} darkened to {y} < {last}"
                );
                last = y;
            }
        }
    }

    /// Grey in, grey out: any chroma the pipeline invents on a neutral is a
    /// tint across the whole picture -- and a chroma *gain* that was written as
    /// a shift, or applied anywhere but about 128, is exactly that tint, which
    /// is why the vivid preset is in this loop.
    #[test]
    fn neutral_input_stays_neutral() {
        for (transfer, preset) in [Transfer::Pq, Transfer::Hlg]
            .into_iter()
            .flat_map(|t| Preset::ALL.map(|p| (t, p)))
        {
            let mapper = ToneMapper::new(transfer, preset, None);
            for code in (16..=235u8).step_by(7) {
                let (_, u, v) = mapped(&mapper, code, 128, 128);
                assert!(
                    u.abs_diff(128) <= 2 && v.abs_diff(128) <= 2,
                    "{preset:?} {transfer:?}: grey {code} tinted to U{u} V{v}"
                );
            }
        }
    }

    /// HLG's own anchors. Its signal is relative, so what is worth pinning is
    /// where the OOTF puts it: black at black and peak signal at the assumed
    /// display peak, hence SDR white -- to the grid's rounding, since code 235
    /// sits between two luma nodes and the one below it is a hair under peak.
    ///
    /// BT.2408 HLG diffuse white is signal 0.75, and it does *not* land on the
    /// PQ side's 203-nit code: relative is the point of HLG, so 0.75 is 19% of
    /// whatever peak the display has -- 76 cd/m^2 of the standard preset's 400,
    /// against PQ's absolute 203 -- and it comes out darker. That is the
    /// standard's own behaviour and the number to watch: if the OOTF's system
    /// gamma stops following the assumed peak, this is what moves, and it moves
    /// per preset because the peak is what a preset picks.
    #[test]
    fn hlg_anchors_are_sane() {
        for (preset, expected) in [
            (Preset::Reference, 170u8),
            (Preset::Standard, 170),
            (Preset::Vivid, 170),
        ] {
            let mapper = ToneMapper::new(Transfer::Hlg, preset, None);
            let code = |signal: f32| byte(16.0 + 219.0 * signal);
            let (black, _, _) = mapped(&mapper, code(0.0), 128, 128);
            let (diffuse, _, _) = mapped(&mapper, code(0.75), 128, 128);
            let (white, _, _) = mapped(&mapper, code(1.0), 128, 128);
            assert!(black.abs_diff(16) <= 3, "{preset:?} HLG black -> {black}");
            assert!(white.abs_diff(235) <= 3, "{preset:?} HLG peak -> {white}");
            assert!(
                diffuse.abs_diff(expected) <= 3,
                "{preset:?} HLG diffuse white -> {diffuse}, expected {expected}"
            );
        }
    }

    /// A short plane is left alone rather than half mapped, and a frame with no
    /// size at all returns instead of panicking on a zero-length band.
    #[test]
    fn a_frame_that_does_not_measure_up_is_untouched() {
        let mapper = ToneMapper::new(Transfer::Pq, Preset::default(), None);
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
        let mapper = ToneMapper::new(Transfer::Pq, Preset::default(), None);
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
        let mapper = ToneMapper::new(Transfer::Pq, Preset::default(), None);
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

    /// The paid debt: the film's own declared peak is what the reference
    /// rendition converts at. 1759 cd/m^2 is a real number -- the MaxCLL of a
    /// real HDR10 grade, which the container never says
    /// and the HEVC SEI does -- and every anchor moves with it, downward,
    /// because a peak this far above the 1000 an undeclared file is assumed at
    /// is *headroom*: the same code is a smaller fraction of a brighter peak.
    ///
    /// The row that matters most is 1000 cd/m^2. Told 1000, Method A puts it at
    /// SDR white and everything above it is gone; told the film's 1759 it lands
    /// at 215 and the 759 nits above it are still a picture. That is the whole
    /// of what reading the metadata buys.
    #[test]
    fn the_reference_rendition_converts_at_the_films_own_peak() {
        const FILM: f32 = 1759.0;
        let mapper = ToneMapper::new(Transfer::Pq, Preset::Reference, Some(FILM));
        assert_eq!(
            mapper.peak(),
            FILM,
            "the table was built at the film's peak"
        );
        // Anchors, cd/m^2 -> limited-range SDR luma, at 1759 rather than 1000:
        // black is still black, the film's own peak is SDR white, and BT.2408
        // diffuse white lands at 145 instead of 166.
        for (nits, expected) in [
            (0.0, 16u8),
            (203.0, 145),
            (1000.0, 215),
            (FILM, 235),
            (10000.0, 235),
        ] {
            let code = byte(pq_code(nits));
            let (y, _, _) = mapped(&mapper, code, 128, 128);
            assert!(
                y.abs_diff(expected) <= 3,
                "{nits} cd/m^2 (code {code}) -> {y}, expected {expected} at a {FILM} peak"
            );
        }
        // ...and the direction, on every code rather than on the anchors: a
        // higher assumed peak is never brighter, and somewhere it is plainly
        // darker. The fallback is what the same file gets with its metadata
        // stripped, which is the control.
        let fallback = ToneMapper::new(Transfer::Pq, Preset::Reference, None);
        assert_eq!(fallback.peak(), 1000.0, "an undeclared file's assumption");
        let mut lower = 0;
        for code in 0..=255u8 {
            let (declared, _, _) = mapped(&mapper, code, 128, 128);
            let (assumed, _, _) = mapped(&fallback, code, 128, 128);
            assert!(
                declared <= assumed,
                "code {code}: the film's peak lifted it, {declared} over {assumed}"
            );
            lower += u32::from(declared < assumed);
        }
        assert!(lower > 128, "only {lower} of 256 codes moved at all");
        // HLG's OOTF follows the same peak, and the black guard has to hold
        // above 1000 as well as below it: a system gamma over 1 makes the
        // `ys^(gamma - 1)` term zero rather than infinite, and a hole where
        // black belongs is the failure either way.
        let hlg = ToneMapper::new(Transfer::Hlg, Preset::Reference, Some(FILM));
        let (black, _, _) = mapped(&hlg, byte(16.0), 128, 128);
        let (near_black, _, _) = mapped(&hlg, byte(16.0 + 219.0 * 0.02), 128, 128);
        let (white, _, _) = mapped(&hlg, 235, 128, 128);
        assert!(black.abs_diff(16) <= 3, "HLG black at {FILM} -> {black}");
        assert!(near_black > 16, "HLG near-black at {FILM} -> {near_black}");
        assert!(white.abs_diff(235) <= 3, "HLG peak at {FILM} -> {white}");
    }

    /// The composition rule on [`Preset`], asserted: the two fixed renditions
    /// are deaf to the metadata, byte for byte, and the reference one is not.
    /// A "read the peak" change that reached all three would make the same pick
    /// mean a different exposure on every file, and this is what catches it.
    #[test]
    fn only_the_reference_rendition_reads_the_declared_peak() {
        for transfer in [Transfer::Pq, Transfer::Hlg] {
            for preset in Preset::ALL {
                let declared = ToneMapper::new(transfer, preset, Some(1759.0));
                let assumed = ToneMapper::new(transfer, preset, None);
                let deaf = preset != Preset::Reference;
                assert_eq!(
                    declared.peak() == assumed.peak(),
                    deaf,
                    "{preset:?} {transfer:?}: peak {} against {}",
                    declared.peak(),
                    assumed.peak()
                );
                assert_eq!(
                    declared.luma == assumed.luma && declared.chroma == assumed.chroma,
                    deaf,
                    "{preset:?} {transfer:?}: the tables"
                );
            }
        }
    }

    /// A declared peak is a number out of a stranger's file, so the two ways it
    /// can be nonsense are checked rather than divided by: a peak under SDR
    /// white would clip the whole picture to white, and a NaN -- which no
    /// comparison admits, hence no clamp catches -- would build a table of
    /// zeroes, i.e. a black frame. Both fall back to the rendition's own.
    #[test]
    fn a_nonsense_declared_peak_is_not_believed() {
        for junk in [0.0, 1.0, 99.0, 10_001.0, f32::NAN, f32::INFINITY] {
            let mapper = ToneMapper::new(Transfer::Pq, Preset::Reference, Some(junk));
            assert_eq!(mapper.peak(), 1000.0, "declared {junk} was believed");
            let (white, _, _) = mapped(&mapper, byte(pq_code(1000.0)), 128, 128);
            assert!(
                white.abs_diff(235) <= 3,
                "declared {junk}: 1000 nits -> {white}"
            );
        }
        // ...and the range's own ends are believed, which is what says the
        // guard rejects nonsense rather than everything unusual.
        for good in [100.0, 4000.0, 10_000.0] {
            let mapper = ToneMapper::new(Transfer::Pq, Preset::Reference, Some(good));
            assert_eq!(mapper.peak(), good, "declared {good} was refused");
        }
    }
}
