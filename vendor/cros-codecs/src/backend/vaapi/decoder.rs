// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Context as AnyhowContext;
use libva::{
    Buffer, Context, Display, Picture, PictureEnd, PictureNew, PictureSync, Surface,
    SurfaceMemoryDescriptor, VaError,
};

use crate::decoder::stateless::StatelessBackendResult;
use crate::decoder::stateless::StatelessCodec;
use crate::decoder::stateless::StatelessDecoderBackend;
use crate::decoder::stateless::StatelessDecoderBackendPicture;
use crate::decoder::DecodedHandle as DecodedHandleTrait;
use crate::decoder::StreamInfo;
use crate::video_frame::VideoFrame;
use crate::DecodedFormat;
use crate::Rect;
use crate::Resolution;

/// A decoded frame handle.
pub(crate) type DecodedHandle<V> = Rc<RefCell<VaapiDecodedHandle<V>>>;

/// Gets the VASurfaceID for the given `picture`.
pub(crate) fn va_surface_id<V: VideoFrame>(
    handle: &Option<DecodedHandle<V>>,
) -> libva::VASurfaceID {
    match handle {
        None => libva::VA_INVALID_SURFACE,
        Some(handle) => handle.borrow().surface().id(),
    }
}

impl<V: VideoFrame> DecodedHandleTrait for DecodedHandle<V> {
    type Frame = V;

    fn video_frame(&self) -> Arc<Self::Frame> {
        self.borrow().backing_frame.clone()
    }

    fn coded_resolution(&self) -> Resolution {
        self.borrow().surface().size().into()
    }

    fn display_resolution(&self) -> Resolution {
        self.borrow().display_resolution
    }

    fn timestamp(&self) -> u64 {
        self.borrow().timestamp()
    }

    fn is_ready(&self) -> bool {
        self.borrow().state.is_ready().unwrap_or(true)
    }

    fn sync(&self) -> anyhow::Result<()> {
        self.borrow_mut().sync().context("while syncing picture")?;

        Ok(())
    }
}

/// A trait for providing the basic information needed to setup libva for decoding.
pub(crate) trait VaStreamInfo {
    /// Returns the VA profile of the stream.
    fn va_profile(&self) -> anyhow::Result<i32>;
    /// Returns the RT format of the stream.
    fn rt_format(&self) -> anyhow::Result<u32>;
    /// Returns the minimum number of surfaces required to decode the stream.
    fn min_num_surfaces(&self) -> usize;
    /// Returns the coded size of the surfaces required to decode the stream.
    fn coded_size(&self) -> Resolution;
    /// Returns the visible rectangle within the coded size for the stream.
    fn visible_rect(&self) -> Rect;
}

/// Rendering state of a VA picture.
enum PictureState<M: SurfaceMemoryDescriptor> {
    Ready(Picture<PictureSync, Surface<M>>),
    Pending(Picture<PictureEnd, Surface<M>>),
    // Only set in sync when we take ownership of the VA picture.
    Invalid,
}

impl<M: SurfaceMemoryDescriptor> PictureState<M> {
    /// Make sure that all pending operations on the picture have completed.
    fn sync(&mut self) -> Result<(), VaError> {
        let res;

        (*self, res) = match std::mem::replace(self, PictureState::Invalid) {
            state @ PictureState::Ready(_) => (state, Ok(())),
            PictureState::Pending(picture) => match picture.sync() {
                Ok(picture) => (PictureState::Ready(picture), Ok(())),
                Err((e, picture)) => (PictureState::Pending(picture), Err(e)),
            },
            PictureState::Invalid => unreachable!(),
        };

        res
    }

    fn surface(&self) -> &Surface<M> {
        match self {
            PictureState::Ready(picture) => picture.surface(),
            PictureState::Pending(picture) => picture.surface(),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn timestamp(&self) -> u64 {
        match self {
            PictureState::Ready(picture) => picture.timestamp(),
            PictureState::Pending(picture) => picture.timestamp(),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn is_ready(&self) -> Result<bool, VaError> {
        match self {
            PictureState::Ready(_) => Ok(true),
            PictureState::Pending(picture) => picture
                .surface()
                .query_status()
                .map(|s| s == libva::VASurfaceStatus::VASurfaceReady),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn new_from_same_surface(&self, timestamp: u64) -> Picture<PictureNew, Surface<M>> {
        match &self {
            PictureState::Ready(picture) => Picture::new_from_same_surface(timestamp, picture),
            PictureState::Pending(picture) => Picture::new_from_same_surface(timestamp, picture),
            PictureState::Invalid => unreachable!(),
        }
    }
}

/// VA-API backend handle.
///
/// This includes the VA picture which can be pending rendering or complete, as well as useful
/// meta-information.
pub struct VaapiDecodedHandle<V: VideoFrame> {
    backing_frame: Arc<V>,
    state: PictureState<<V as VideoFrame>::MemDescriptor>,
    /// Actual resolution of the visible rectangle in the decoded buffer.
    display_resolution: Resolution,
    /// LOCAL PATCH (see /Cargo.toml [patch.crates-io]): the second surface an
    /// AV1 picture with `apply_grain` set is *displayed* from -- the driver
    /// synthesizes the grain into it and leaves the reconstructed picture, which
    /// is what later frames reference, untouched in `state`'s own surface.
    /// `None` for every picture that asks for no grain, which is every picture
    /// of every other codec.
    display: Option<(Arc<V>, Surface<<V as VideoFrame>::MemDescriptor>)>,
}

impl<V: VideoFrame> VaapiDecodedHandle<V> {
    /// Creates a new pending handle on `surface_id`.
    fn new(picture: VaapiPicture<V>, display_resolution: Resolution) -> anyhow::Result<Self> {
        let backing_frame = picture.backing_frame;
        let display = picture.display;
        let picture = picture.picture.begin()?.render()?.end()?;
        Ok(Self {
            backing_frame: backing_frame,
            state: PictureState::Pending(picture),
            display_resolution: display_resolution,
            display: display,
        })
    }

    fn sync(&mut self) -> Result<(), VaError> {
        self.state.sync()?;
        // LOCAL PATCH (see /Cargo.toml [patch.crates-io]): the grain is
        // synthesized into a surface of its own, and it is that one the pixels
        // are read back off, so waiting only on the reconstructed picture would
        // hand out a half-written frame.
        if let Some((_, surface)) = &self.display {
            surface.sync()?;
        }
        Ok(())
    }

    /// Creates a new picture from the surface backing the current one. Useful for interlaced
    /// decoding. TODO: Do we need this for other purposes? We don't intend to support interlaced.
    pub(crate) fn new_picture_from_same_surface(&self, timestamp: u64) -> VaapiPicture<V> {
        VaapiPicture {
            picture: self.state.new_from_same_surface(timestamp),
            backing_frame: self.backing_frame.clone(),
            // Interlaced H.264 only, which no codec here asks film grain of.
            display: None,
        }
    }

    // LOCAL PATCH (see /Cargo.toml [patch.crates-io]): made public so a client can
    // read frames back with vaGetImage. Mapping the client-allocated DMA-BUF
    // directly is a 44 MB/s uncached PCIe read on a discrete GPU; the driver's own
    // copy path is ~100x faster.
    pub fn surface(&self) -> &Surface<<V as VideoFrame>::MemDescriptor> {
        self.state.surface()
    }

    /// LOCAL PATCH (see /Cargo.toml [patch.crates-io]): the surface a *viewer*
    /// wants, which is the grain-synthesized one where there is one and the
    /// reconstructed picture everywhere else. [`Self::surface`] stays the
    /// reconstructed one on purpose: that is what the reference lists point at.
    pub fn display_surface(&self) -> &Surface<<V as VideoFrame>::MemDescriptor> {
        match &self.display {
            Some((_, surface)) => surface,
            None => self.state.surface(),
        }
    }

    /// Returns the timestamp of this handle.
    fn timestamp(&self) -> u64 {
        self.state.timestamp()
    }
}

pub struct VaapiBackend<V: VideoFrame> {
    pub display: Rc<Display>,
    pub context: Rc<Context>,
    stream_info: StreamInfo,
    // TODO: We should try to support context reuse
    _supports_context_reuse: bool,
    _phantom_data: PhantomData<V>,
}

impl<V: VideoFrame> VaapiBackend<V> {
    pub(crate) fn new(display: Rc<libva::Display>, supports_context_reuse: bool) -> Self {
        // LOCAL PATCH (see /Cargo.toml [patch.crates-io]): upstream probes with a
        // 16x16 context, which Mesa radeonsi rejects with VA_STATUS_ERROR_UNIMPLEMENTED
        // (measured: 16x16 and 48x48 fail, 64x64 up succeed), and the `expect` below
        // then aborts. This placeholder is replaced by `new_sequence` anyway.
        let init_stream_info = StreamInfo {
            format: DecodedFormat::NV12,
            coded_resolution: Resolution::from((64, 64)),
            display_resolution: Resolution::from((64, 64)),
            min_num_frames: 1,
        };
        let config = display
            .create_config(
                vec![libva::VAConfigAttrib {
                    type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                    value: libva::VA_RT_FORMAT_YUV420,
                }],
                libva::VAProfile::VAProfileH264Main,
                libva::VAEntrypoint::VAEntrypointVLD,
            )
            .expect("Could not create initial VAConfig!");
        let context = display
            .create_context::<<V as VideoFrame>::MemDescriptor>(
                &config,
                init_stream_info.coded_resolution.width,
                init_stream_info.coded_resolution.height,
                None,
                true,
            )
            .expect("Could not create initial VAContext!");
        Self {
            display: display,
            context: context,
            _supports_context_reuse: supports_context_reuse,
            stream_info: init_stream_info,
            _phantom_data: Default::default(),
        }
    }

    pub(crate) fn new_sequence<StreamData>(
        &mut self,
        stream_params: &StreamData,
    ) -> StatelessBackendResult<()>
    where
        for<'a> &'a StreamData: VaStreamInfo,
    {
        self.stream_info.display_resolution = Resolution::from(stream_params.visible_rect());
        self.stream_info.coded_resolution = stream_params.coded_size().clone();
        self.stream_info.min_num_frames = stream_params.min_num_surfaces();

        // TODO: Handle context re-use
        // LOCAL PATCH (see /Cargo.toml [patch.crates-io]): the RT format comes
        // from the stream rather than being assumed 8-bit 4:2:0, which is what
        // an HEVC Main 10 stream needs -- its surfaces are P010 and a
        // YUV420-only config rejects them. This is upstream's own TODO, and
        // `VaStreamInfo::rt_format` is upstream's own answer to it.
        let rt_format = stream_params.rt_format().unwrap_or(libva::VA_RT_FORMAT_YUV420);
        let config = self
            .display
            .create_config(
                vec![libva::VAConfigAttrib {
                    type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                    value: rt_format,
                }],
                stream_params.va_profile().map_err(|_| anyhow!("Could not get VAProfile!"))?,
                libva::VAEntrypoint::VAEntrypointVLD,
            )
            .map_err(|_| anyhow!("Could not create VAConfig!"))?;
        let context = self
            .display
            .create_context::<<V as VideoFrame>::MemDescriptor>(
                &config,
                self.stream_info.coded_resolution.width,
                self.stream_info.coded_resolution.height,
                None,
                true,
            )
            .map_err(|_| anyhow!("Could not create VAContext!"))?;
        self.context = context;

        Ok(())
    }

    pub(crate) fn process_picture<Codec: StatelessCodec>(
        &mut self,
        picture: VaapiPicture<V>,
    ) -> StatelessBackendResult<<Self as StatelessDecoderBackend>::Handle>
    where
        Self: StatelessDecoderBackendPicture<Codec>,
        for<'a> &'a Codec::FormatInfo: VaStreamInfo,
    {
        Ok(Rc::new(RefCell::new(VaapiDecodedHandle::new(
            picture,
            self.stream_info.display_resolution.clone(),
        )?)))
    }
}

/// Shortcut for pictures used for the VAAPI backend.
pub struct VaapiPicture<V: VideoFrame> {
    picture: Picture<PictureNew, Surface<V::MemDescriptor>>,
    backing_frame: Arc<V>,
    display: Option<(Arc<V>, Surface<V::MemDescriptor>)>,
}

impl<V: VideoFrame> VaapiPicture<V> {
    pub fn new(timestamp: u64, context: Rc<Context>, backing_frame: V) -> Self {
        let display = context.display();
        let surface = backing_frame
            .to_native_handle(display)
            .expect("Failed to export video frame to vaapi picture!")
            .into();
        Self {
            backing_frame: Arc::new(backing_frame),
            picture: Picture::new(timestamp, context, surface),
            display: None,
        }
    }

    /// LOCAL PATCH (see /Cargo.toml [patch.crates-io]): gives this picture the
    /// second surface an AV1 frame with `apply_grain` set is displayed from. The
    /// driver writes the reconstructed picture into the render target as always
    /// and the grain-synthesized one into this; a driver told to apply grain with
    /// no such target refuses the picture outright (radeonsi answers
    /// `vaEndPicture` with VA_STATUS_ERROR_INVALID_SURFACE).
    pub fn set_display_frame(&mut self, display: &Rc<Display>, frame: V) {
        let surface = frame
            .to_native_handle(display)
            .expect("Failed to export film grain target to vaapi picture!")
            .into();
        self.display = Some((Arc::new(frame), surface));
    }

    /// The id [`Self::set_display_frame`] handed over, or `VA_INVALID_SURFACE`
    /// when this picture wants no grain -- which is what the picture parameter
    /// buffer's `current_display_picture` takes either way.
    pub fn display_surface_id(&self) -> libva::VASurfaceID {
        match &self.display {
            Some((_, surface)) => surface.id(),
            None => libva::VA_INVALID_SURFACE,
        }
    }

    pub fn surface(&self) -> &Surface<V::MemDescriptor> {
        self.picture.surface()
    }

    pub fn add_buffer(&mut self, buffer: Buffer) {
        self.picture.add_buffer(buffer)
    }
}

impl<V: VideoFrame> StatelessDecoderBackend for VaapiBackend<V> {
    type Handle = DecodedHandle<V>;

    fn stream_info(&self) -> Option<&StreamInfo> {
        Some(&self.stream_info)
    }

    fn reset_backend(&mut self) -> anyhow::Result<()> {
        //TODO(bchoobineh): Implement VAAPI DRC
        Ok(())
    }
}
