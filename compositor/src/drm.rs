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
use smithay::backend::renderer::element::RenderElementStates;
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
    pending_flip: Option<PendingFlip>,
    power: OutputPower,
    prepared: Option<PreparedFrame>,
}

struct PreparedFrame {
    content: Content,
    damaged: bool,
    states: RenderElementStates,
    fence: Option<SyncPoint>,
}

impl PreparedFrame {
    fn ready(&self) -> bool {
        self.fence.as_ref().is_none_or(SyncPoint::is_reached)
    }
}

enum PendingFlip {
    Awaited,
    Expired,
    Cleared,
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
            pending_flip: None,
            power: OutputPower::On,
            prepared: None,
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
        windows: &[crate::state::Window],
        placements: &[frame::Placement],
        content: Content,
        force_full_redraw: bool,
    ) -> Result<bool, Box<dyn Error>> {
        if let Some(frame) = &self.prepared
            && frame.content == content
        {
            return Ok(frame.ready());
        }
        self.prepared = None;
        if force_full_redraw {
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
                FrameFlags::DEFAULT,
            )
            .map_err(|err| err.to_string())?;
        let fence = match &frame.primary_element {
            PrimaryPlaneElement::Swapchain(element)
                if !frame.is_empty
                    && (content == Content::Protected || frame.needs_sync())
                    && !element.sync.is_reached() =>
            {
                Some(element.sync.clone())
            }
            _ => None,
        };
        // A modeset can block on an input fence even when KMS accepts it.
        // Keep one frame while the fence is pending so the cover timer can run.
        let ready = fence.is_none();
        self.prepared = Some(PreparedFrame {
            content,
            damaged: !frame.is_empty,
            states: frame.states,
            fence,
        });
        Ok(ready)
    }

    pub(crate) fn discard_prepared(&mut self) {
        self.prepared = None;
    }

    pub(crate) fn discard_session_frame(&mut self) {
        // A later client commit must keep its own presentation feedback.
        if self
            .prepared
            .as_ref()
            .is_some_and(|frame| frame.content == Content::Session)
        {
            self.discard_prepared();
        }
    }

    pub(crate) fn submit(&mut self) -> Result<(bool, RenderElementStates, bool), Box<dyn Error>> {
        let frame = self.prepared.take().ok_or("no prepared DRM frame")?;
        if frame.damaged {
            self.surface
                .queue_frame(frame.content)
                .map_err(|err| err.to_string())?;
            self.pending_flip = Some(PendingFlip::Awaited);
        }
        Ok((frame.damaged, frame.states, frame.fence.is_some()))
    }

    pub(crate) fn ready_to_render(&self) -> bool {
        // Leave timed-out flips in flight so a late completion keeps its original tag.
        self.can_blank() && self.pending_flip.is_none()
    }

    pub(crate) fn can_blank(&self) -> bool {
        self.active && self.power == OutputPower::On
    }

    pub(crate) fn blank(&mut self) -> Result<(), smithay::backend::drm::DrmError> {
        self.surface.with_compositor(DrmCompositor::clear)?;
        self.discard_prepared();
        // clear retires the frame in Smithay. A queued event can still arrive
        // on this descriptor, so reserve its slot until the notifier drains it.
        if self.pending_flip.is_some() {
            self.pending_flip = Some(PendingFlip::Cleared);
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
        if self.active && self.power == OutputPower::WakeRequested && self.pending_flip.is_none() {
            self.surface.reset_buffers();
            self.power = OutputPower::On;
            return true;
        }
        false
    }

    pub(crate) fn complete(&mut self) -> DrmFlip {
        let Some(pending) = self.pending_flip.take() else {
            return DrmFlip::Ignored;
        };
        if matches!(pending, PendingFlip::Cleared) {
            return DrmFlip::Expired;
        }
        match (pending, self.surface.frame_submitted()) {
            (PendingFlip::Awaited, Ok(Some(content))) => DrmFlip::Presented(content),
            (PendingFlip::Expired | PendingFlip::Cleared, Ok(Some(_))) => DrmFlip::Expired,
            (_, Ok(None)) => {
                tracing::warn!("DRM completion did not have a submitted frame");
                DrmFlip::Ignored
            }
            (_, Err(err)) => {
                tracing::warn!(%err, "DRM completion failed");
                DrmFlip::Ignored
            }
        }
    }

    pub(crate) fn expire_flip(&mut self) {
        if matches!(self.pending_flip, Some(PendingFlip::Awaited)) {
            self.pending_flip = Some(PendingFlip::Expired);
        }
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
mod tests {
    use std::os::fd::OwnedFd;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use smithay::backend::renderer::sync::{Fence, Interrupted};

    use super::*;

    #[derive(Debug)]
    struct TestFence(Arc<AtomicBool>);

    impl Fence for TestFence {
        fn is_signaled(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }

        fn wait(&self) -> Result<(), Interrupted> {
            panic!("the event loop must poll a pending fence")
        }

        fn is_exportable(&self) -> bool {
            false
        }

        fn export(&self) -> Option<OwnedFd> {
            None
        }
    }

    #[test]
    fn req_lock_011_prepared_cover_polls_its_fence_without_waiting() {
        let signaled = Arc::new(AtomicBool::new(false));
        let frame = PreparedFrame {
            content: Content::Protected,
            damaged: true,
            states: RenderElementStates::default(),
            fence: Some(SyncPoint::from(TestFence(signaled.clone()))),
        };
        assert!(!frame.ready());
        signaled.store(true, Ordering::Relaxed);
        assert!(frame.ready());
    }
}
