//! One connector on the logind seat, through Smithay's safe GBM/GLES backend.

use std::error::Error;

use skorupa_compositor_core::lock::Content;
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, DrmEventTime, DrmNode, NodeType,
};
use smithay::backend::input::{InputEvent, KeyState, KeyboardKeyEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::ImportMemWl;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer, gbm::GbmGlesBackend};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{all_gpus, primary_gpu};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::drm::control::{Device, ModeTypeFlags, connector, crtc};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::protocol::wl_shm::Format;
use smithay::utils::DeviceFd;

use crate::session_lock::read_lock_clocks;
use crate::{Compositor, Config, frame, state::State};

type Api = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type Renderer<'a> = MultiRenderer<'a, 'a, Api, Api>;
type Element<'a> = WaylandSurfaceRenderElement<Renderer<'a>>;
type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;

pub(crate) struct Drm {
    manager: DrmOutputManager<Allocator, Exporter, Content, DrmDeviceFd>,
    surface: DrmOutput<Allocator, Exporter, Content, DrmDeviceFd>,
    gpu: GpuManager<Api>,
    node: DrmNode,
    active: bool,
    power: OutputPower,
    frames: DrmFrames,
}

#[derive(Default)]
struct DrmFrames {
    pending_flip: Option<PendingFlip>,
    prepared: Option<PreparedFrame>,
}

struct PreparedFrame {
    content: Content,
    rendered: frame::RenderedFrame,
    fence: Option<SyncPoint>,
}

impl PreparedFrame {
    fn ready(&self) -> bool {
        self.fence.as_ref().is_none_or(SyncPoint::is_reached)
    }
}

impl DrmFrames {
    fn submission(&self, content: Content) -> Option<Submission> {
        match self.pending_flip {
            None => Some(Submission::Queue),
            Some(PendingFlip::Expired | PendingFlip::Retired) if content == Content::Session => {
                Some(Submission::Commit)
            }
            Some(_) => None,
        }
    }

    fn expire_flip(&mut self) {
        if matches!(self.pending_flip, Some(PendingFlip::Awaited)) {
            self.pending_flip = Some(PendingFlip::Expired);
        }
    }

    fn queue_redraw(&mut self) {
        if let Some(frame) = &mut self.prepared {
            frame.rendered.redraw_needed = true;
        }
    }

    fn submit(
        &mut self,
        submit: impl FnOnce(Submission, Content) -> Result<(), Box<dyn Error>>,
    ) -> Result<frame::RenderedFrame, frame::RenderFailure> {
        let frame = self
            .prepared
            .take()
            .ok_or_else(|| Box::<dyn Error>::from("no prepared DRM frame"))?;
        if frame.rendered.damaged {
            // A newer commit already requested a separate frame. Keep that
            // request even when submission of this older frame fails.
            let failure = |cause| frame::RenderFailure {
                cause,
                redraw_needed: frame.rendered.redraw_needed,
            };
            let mode = self
                .submission(frame.content)
                .ok_or_else(|| failure("waiting for DRM flip".into()))?;
            submit(mode, frame.content).map_err(failure)?;
            self.pending_flip = Some(match mode {
                Submission::Queue => PendingFlip::Awaited,
                // Smithay replaced its pending frame. Its old kernel event can still arrive.
                Submission::Commit => PendingFlip::Retired,
            });
        }
        Ok(frame.rendered)
    }

    fn complete<E: std::fmt::Display>(
        &mut self,
        submitted: impl FnOnce() -> Result<Option<Content>, E>,
    ) -> DrmFlip {
        match self.pending_flip.take() {
            None => DrmFlip::Ignored,
            Some(PendingFlip::Retired) => DrmFlip::Expired,
            Some(pending) => match submitted() {
                Ok(Some(content)) if matches!(pending, PendingFlip::Awaited) => {
                    DrmFlip::Presented(content)
                }
                Ok(_) if matches!(pending, PendingFlip::Expired) => DrmFlip::Expired,
                Ok(_) => {
                    tracing::warn!("DRM completion did not have a submitted frame");
                    DrmFlip::Ignored
                }
                Err(err) => {
                    tracing::warn!(%err, "DRM completion failed");
                    DrmFlip::Ignored
                }
            },
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum Submission {
    Queue,
    Commit,
}

enum PendingFlip {
    Awaited,
    /// The notifier and Smithay may both still hold the old flip.
    Expired,
    /// Only the notifier can still hold the old flip, after `clear` or `commit_frame`.
    Retired,
}

#[derive(PartialEq, Eq)]
enum OutputPower {
    On,
    Off,
    WakeRequested,
}

pub(crate) enum DrmFlip {
    Presented(Content),
    Expired,
    Ignored,
}

pub(crate) struct Devices {
    session: LibSeatSessionNotifier,
    drm: DrmDeviceNotifier,
    input: Libinput,
    crtc: crtc::Handle,
}

fn select_connector(
    drm: &DrmDevice,
) -> Result<
    (
        connector::Info,
        crtc::Handle,
        smithay::reexports::drm::control::Mode,
    ),
    Box<dyn Error>,
> {
    let resources = drm.resource_handles()?;
    let (connector, crtc) = resources
        .connectors()
        .iter()
        .filter_map(|handle| drm.get_connector(*handle, true).ok())
        .filter(|info| info.state() == connector::State::Connected && !info.modes().is_empty())
        .find_map(|info| {
            let crtc = info
                .encoders()
                .iter()
                .filter_map(|encoder| drm.get_encoder(*encoder).ok())
                .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
                .next()?;
            Some((info, crtc))
        })
        .ok_or("no connected connector with a usable CRTC")?;
    let mode = connector
        .modes()
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .copied()
        .unwrap_or(connector.modes()[0]);
    Ok((connector, crtc, mode))
}

impl Drm {
    pub(crate) fn open() -> Result<(Self, Devices, Output, Config), Box<dyn Error>> {
        let (mut session, notifier) = LibSeatSession::new()?;
        let seat = session.seat();
        let path = primary_gpu(&seat)?
            .or_else(|| all_gpus(&seat).ok()?.into_iter().next())
            .ok_or("no DRM device on this seat")?;
        let card = DrmNode::from_path(&path)?;
        let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
        let fd = session.open(&path, flags)?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, drm_notifier) = DrmDevice::new(fd.clone(), false)?;
        if !drm.is_atomic() {
            return Err("the DRM device does not support atomic modesetting".into());
        }
        let gbm = GbmDevice::new(fd)?;
        let node = card
            .node_with_type(NodeType::Render)
            .and_then(Result::ok)
            .unwrap_or(card);
        let mut gpu =
            GpuManager::<Api>::new(GbmGlesBackend::default()).map_err(|err| err.to_string())?;
        gpu.as_mut().add_node(node, gbm.clone())?;

        let (connector, crtc, mode) = select_connector(&drm)?;
        let wl_mode = Mode::from(mode);
        let (width, height) = connector.size().unwrap_or_default();
        let output = Output::new(
            format!(
                "{}-{}",
                connector.interface().as_str(),
                connector.interface_id()
            ),
            PhysicalProperties {
                size: (i32::try_from(width)?, i32::try_from(height)?).into(),
                subpixel: Subpixel::Unknown,
                make: "Skorupa".into(),
                model: "DRM".into(),
                serial_number: String::new(),
            },
        );
        output.set_preferred(wl_mode);
        output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));
        let config = Config {
            output_size: (wl_mode.size.w, wl_mode.size.h),
            refresh_mhz: wl_mode.refresh,
            ..Config::default()
        };

        let mut renderer = gpu.single_renderer(&node).map_err(|err| err.to_string())?;
        let formats = renderer
            .as_mut()
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect::<FormatSet>();
        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node).into());
        let mut manager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            [Fourcc::Abgr8888, Fourcc::Argb8888, Fourcc::Xrgb8888],
            formats,
        );
        let surface = manager
            .lock()
            .initialize_output::<_, Element<'_>>(
                crtc,
                mode,
                &[connector.handle()],
                &output,
                None,
                &mut renderer,
                &DrmOutputRenderElements::default(),
            )
            .map_err(|err| err.to_string())?;
        drop(renderer);

        let devices = Devices::new(session, notifier, drm_notifier, &seat, crtc)?;
        let drm = Self {
            manager,
            surface,
            gpu,
            node,
            active: true,
            frames: DrmFrames::default(),
            power: OutputPower::On,
        };
        Ok((drm, devices, output, config))
    }

    pub(crate) fn shm_formats(&mut self) -> Result<Vec<Format>, String> {
        let renderer = self
            .gpu
            .single_renderer(&self.node)
            .map_err(|err| err.to_string())?;
        Ok(renderer.shm_formats().collect())
    }

    pub(crate) fn prepare(
        &mut self,
        output: &Output,
        windows: &[crate::state::Window],
        placements: &[frame::Placement],
        content: Content,
        force_full_redraw: bool,
    ) -> Result<bool, Box<dyn Error>> {
        if let Some(frame) = &self.frames.prepared
            && frame.content == content
        {
            return Ok(frame.ready());
        }
        self.frames.prepared = None;
        let synchronous = self.frames.submission(content) == Some(Submission::Commit);
        // Redraw even an unchanged scene on the first recovery commit.
        if force_full_redraw || matches!(self.frames.pending_flip, Some(PendingFlip::Expired)) {
            self.surface
                .with_compositor(DrmCompositor::reset_buffer_ages);
        }
        let mut renderer = self
            .gpu
            .single_renderer(&self.node)
            .map_err(|err| err.to_string())?;
        let elements = frame::elements(&mut renderer, windows, placements);
        let frame = self
            .surface
            .render_frame(
                &mut renderer,
                &elements,
                frame::background(content),
                if synchronous {
                    FrameFlags::empty()
                } else {
                    FrameFlags::DEFAULT
                },
            )
            .map_err(|err| err.to_string())?;
        let fence = match &frame.primary_element {
            PrimaryPlaneElement::Swapchain(element)
                if !frame.is_empty
                    && (content == Content::Protected || synchronous || frame.needs_sync())
                    && !element.sync.is_reached() =>
            {
                Some(element.sync.clone())
            }
            _ => None,
        };
        // A modeset can block on an input fence even when KMS accepts it.
        // Keep one frame while the fence is pending so the cover timer can run.
        let ready = fence.is_none();
        self.frames.prepared = Some(PreparedFrame {
            content,
            rendered: frame::RenderedFrame::new(
                output,
                windows,
                content,
                !frame.is_empty,
                frame.states,
            ),
            fence,
        });
        Ok(ready)
    }

    pub(crate) fn discard_prepared(&mut self) {
        self.frames.prepared = None;
    }

    pub(crate) fn queue_redraw(&mut self) {
        self.frames.queue_redraw();
    }

    pub(crate) fn submit(&mut self) -> Result<frame::RenderedFrame, frame::RenderFailure> {
        self.frames.submit(|mode, content| {
            match mode {
                Submission::Queue => self.surface.queue_frame(content),
                Submission::Commit => self.surface.commit_frame(),
            }
            .map_err(|err| err.to_string().into())
        })
    }

    pub(crate) fn ready_to_render(&self, content: Content) -> bool {
        self.can_blank() && self.frames.submission(content).is_some()
    }

    pub(crate) fn committed_synchronously(&self) -> bool {
        matches!(self.frames.pending_flip, Some(PendingFlip::Retired))
    }

    pub(crate) fn can_blank(&self) -> bool {
        self.active && self.power == OutputPower::On
    }

    pub(crate) fn blank(&mut self) -> Result<(), smithay::backend::drm::DrmError> {
        self.surface.with_compositor(DrmCompositor::clear)?;
        self.discard_prepared();
        // clear retires the frame in Smithay. A queued event can still arrive
        // on this descriptor, so reserve its slot until the notifier drains it.
        if self.frames.pending_flip.is_some() {
            self.frames.pending_flip = Some(PendingFlip::Retired);
        }
        self.power = OutputPower::Off;
        Ok(())
    }

    pub(crate) fn request_wake(&mut self) -> bool {
        if self.power != OutputPower::On {
            self.power = OutputPower::WakeRequested;
        }
        self.wake_if_ready()
    }

    pub(crate) fn wake_if_ready(&mut self) -> bool {
        if self.active
            && self.power == OutputPower::WakeRequested
            && self.frames.pending_flip.is_none()
        {
            self.surface.reset_buffers();
            self.power = OutputPower::On;
            return true;
        }
        false
    }

    pub(crate) fn complete(&mut self) -> DrmFlip {
        self.frames.complete(|| self.surface.frame_submitted())
    }

    pub(crate) fn expire_flip(&mut self) {
        self.frames.expire_flip();
    }

    pub(crate) fn pause(&mut self) {
        self.active = false;
        self.discard_prepared();
        self.expire_flip();
        self.manager.pause();
    }

    pub(crate) fn activate(&mut self) -> Result<(), String> {
        self.manager
            .lock()
            .activate(false)
            .map_err(|err| err.to_string())?;
        self.surface.reset_buffers();
        self.active = true;
        self.power = OutputPower::On;
        Ok(())
    }
}

impl Devices {
    fn new(
        seat_session: LibSeatSession,
        session: LibSeatSessionNotifier,
        drm: DrmDeviceNotifier,
        seat: &str,
        crtc: crtc::Handle,
    ) -> Result<Self, Box<dyn Error>> {
        let mut input = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
            seat_session.into(),
        );
        input
            .udev_assign_seat(seat)
            .map_err(|()| "libinput could not assign the seat")?;
        Ok(Self {
            session,
            drm,
            input,
            crtc,
        })
    }

    pub(crate) fn register(
        self,
        handle: &LoopHandle<'_, Compositor>,
    ) -> Result<(), Box<dyn Error>> {
        let Self {
            session,
            drm,
            mut input,
            crtc,
        } = self;
        handle.insert_source(
            LibinputInputBackend::new(input.clone()),
            move |event, (), compositor| {
                if let InputEvent::Keyboard { event } = event {
                    compositor.key_at(
                        event.key_code().raw(),
                        event.state() == KeyState::Pressed,
                        read_lock_clocks(),
                    );
                }
            },
        )?;

        handle.insert_source(session, move |event, (), compositor| {
            let state: &mut State = &mut compositor.state;
            state.lock_clock(read_lock_clocks());
            match event {
                SessionEvent::PauseSession => {
                    input.suspend();
                    state.pause_drm();
                }
                SessionEvent::ActivateSession => {
                    if let Err(err) = input.resume() {
                        tracing::warn!(?err, "libinput resume failed");
                    }
                    state.activate_drm();
                }
            }
        })?;

        handle.insert_source(drm, move |event, metadata, compositor| match event {
            DrmEvent::VBlank(event_crtc) if event_crtc == crtc => {
                let time = metadata.as_ref().and_then(|metadata| match metadata.time {
                    DrmEventTime::Monotonic(time) => Some(time),
                    DrmEventTime::Realtime(_) => None,
                });
                let sequence = metadata.as_ref().map_or(0, |metadata| metadata.sequence);
                let clocks = read_lock_clocks();
                compositor.state.lock_clock(clocks);
                compositor
                    .state
                    .complete_drm(clocks.monotonic, time, sequence);
            }
            DrmEvent::VBlank(_) => {}
            DrmEvent::Error(err) => tracing::warn!(%err, "DRM event failed"),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
