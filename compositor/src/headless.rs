//! Headless backend: the Pixman software renderer drawing into one image in
//! memory. There is no page flip to wait for, so the frame pacer is told that
//! each flip completes at once and throttles to the refresh interval itself.

use std::error::Error;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::output::Output;
use smithay::reexports::pixman::Image;
use smithay::utils::{Buffer, Physical, Rectangle, Size};

use crate::frame::BACKGROUND;

pub type Element = WaylandSurfaceRenderElement<PixmanRenderer>;

pub struct Headless {
    renderer: PixmanRenderer,
    image: Image<'static, 'static>,
    tracker: OutputDamageTracker,
    size: Size<i32, Buffer>,
}

impl Headless {
    /// # Errors
    ///
    /// When Pixman cannot create the renderer or the image.
    pub fn new(output: &Output, size: Size<i32, Physical>) -> Result<Self, Box<dyn Error>> {
        let mut renderer = PixmanRenderer::new()?;
        let size = Size::from((size.w, size.h));
        let image = renderer.create_buffer(Fourcc::Argb8888, size)?;
        Ok(Self {
            renderer,
            image,
            tracker: OutputDamageTracker::from_output(output),
            size,
        })
    }

    pub fn renderer(&mut self) -> &mut PixmanRenderer {
        &mut self.renderer
    }

    /// Draws the elements, front to back. Returns whether anything on the
    /// output changed, and what happened to each element.
    ///
    /// # Errors
    ///
    /// When the image cannot be bound or drawing fails.
    pub fn render(
        &mut self,
        elements: &[Element],
    ) -> Result<(bool, RenderElementStates), Box<dyn Error>> {
        let mut framebuffer = self.renderer.bind(&mut self.image)?;
        // One image that is never swapped, so its content is always one frame old.
        let result = self
            .tracker
            .render_output(
                &mut self.renderer,
                &mut framebuffer,
                1,
                elements,
                BACKGROUND,
            )
            .map_err(|err| err.to_string())?;
        Ok((result.damage.is_some(), result.states))
    }

    /// The last frame as ARGB8888 rows, top to bottom.
    ///
    /// # Errors
    ///
    /// When the image cannot be read back.
    pub fn pixels(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        let framebuffer = self.renderer.bind(&mut self.image)?;
        let region = Rectangle::from_size(self.size);
        let mapping = self
            .renderer
            .copy_framebuffer(&framebuffer, region, Fourcc::Argb8888)?;
        Ok(self.renderer.map_texture(&mapping)?.to_vec())
    }
}
