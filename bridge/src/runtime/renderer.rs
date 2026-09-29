//! Draws Blish HUD's latest frame over the game's.
//!
//! Each frame, just before the game presents, the most recently completed of Blish's two shared
//! textures is drawn over the whole backbuffer with alpha blending.
//!
//! # Shaders
//!
//! `shaders/overlay_vs.cso` and `overlay_ps.cso` are precompiled (fxc, shader model 5):
//!
//! - the vertex shader takes only `SV_VertexID` and emits `SV_Position` plus a `TEXCOORD`,
//!   generating a full-screen triangle from three vertex ids — so no vertex buffer or input
//!   layout is bound
//! - the pixel shader samples texture `t0` (`tex`) with sampler `s0` (`samp`)
//!
//! # Pipeline state
//!
//! Every stage this draw depends on is set explicitly rather than inherited from the game, and
//! nothing that references an outside resource is left bound afterwards. That matters for the
//! backbuffer in particular: the game cannot resize its swapchain while any view of it exists,
//! so the render target view is created per frame and released before returning.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering},
    },
};

use windows::Win32::{
    Foundation::{HANDLE, HWND, TRUE},
    Graphics::{
        Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
        Direct3D11::{
            D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD,
            D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_ZERO, D3D11_COLOR_WRITE_ENABLE_ALL,
            D3D11_COMPARISON_NEVER, D3D11_CULL_NONE, D3D11_FILL_SOLID,
            D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_FLOAT32_MAX, D3D11_RASTERIZER_DESC,
            D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SAMPLER_DESC, D3D11_TEXTURE_ADDRESS_CLAMP,
            D3D11_TEXTURE2D_DESC, D3D11_VIEWPORT, ID3D11BlendState, ID3D11DepthStencilState,
            ID3D11Device, ID3D11DeviceContext, ID3D11DomainShader, ID3D11GeometryShader,
            ID3D11HullShader, ID3D11InputLayout, ID3D11PixelShader, ID3D11RasterizerState,
            ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
        },
        Dxgi::{DXGI_SWAP_CHAIN_DESC, IDXGISwapChain},
    },
};

use super::{link, sys::lock};
use crate::{Error, Result, protocol::Header};

const VERTEX_SHADER: &[u8] = include_bytes!("../../shaders/overlay_vs.cso");
const PIXEL_SHADER: &[u8] = include_bytes!("../../shaders/overlay_ps.cso");

static ENABLED: AtomicBool = AtomicBool::new(true);
static RESET_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set when drawing panicked. The overlay stays off from then on, rather than panicking again
/// every frame.
static BROKEN: AtomicBool = AtomicBool::new(false);
static GAME_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Milestones logged once each, so the log shows how far rendering gets: frames reaching the
/// game window, then Blish's frames actually being drawn.
static SAW_GAME_FRAME: AtomicBool = AtomicBool::new(false);
static DREW_FRAME: AtomicBool = AtomicBool::new(false);

static RENDERER: Mutex<Option<Renderer>> = Mutex::new(None);
/// The last drawing error logged, so a persistent one is reported once rather than per frame.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Restricts drawing to swapchains presenting to this window.
pub fn set_game_window(window: HWND) {
    GAME_WINDOW.store(window.0, Ordering::Relaxed);
}

/// Turns drawing on or off, returning whether it is now on.
pub fn toggle() -> bool {
    !ENABLED.fetch_xor(true, Ordering::Relaxed)
}

/// Whether drawing is on.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) && !BROKEN.load(Ordering::Relaxed)
}

/// Rebuilds all D3D11 state on the next frame.
pub fn request_reset() {
    RESET_REQUESTED.store(true, Ordering::Relaxed);
}

/// Draws the overlay onto `swapchain`'s backbuffer, if it is the game's. Never panics and
/// never fails: problems are logged and the frame is left alone.
pub fn draw(swapchain: &IDXGISwapChain) {
    if !is_enabled() {
        return;
    }

    match catch_unwind(AssertUnwindSafe(|| try_draw(swapchain))) {
        Ok(Ok(())) => {
            if lock(&LAST_ERROR).take().is_some() {
                log::info!("overlay drawing again");
            }
        }
        Ok(Err(error)) => {
            let message = error.to_string();
            let mut last = lock(&LAST_ERROR);
            if last.as_deref() != Some(&message) {
                log::warn!("overlay not drawn: {message}");
                *last = Some(message);
            }
        }
        Err(_) => {
            BROKEN.store(true, Ordering::Relaxed);
            *lock(&RENDERER) = None;
            log::error!("the renderer panicked; the overlay is off until the game restarts");
        }
    }
}

fn try_draw(swapchain: &IDXGISwapChain) -> Result<()> {
    // Other swapchains share this Present — the launcher's, and our own probe.
    if !presents_to_game_window(swapchain)? {
        return Ok(());
    }
    if !SAW_GAME_FRAME.swap(true, Ordering::Relaxed) {
        log::info!("the game is presenting frames to its window");
    }

    let mut renderer = lock(&RENDERER);

    let Some(header) = link::header().filter(Header::has_textures) else {
        // Nothing to draw. Let go of any textures so an exited Blish's are not kept alive.
        if let Some(renderer) = renderer.as_mut() {
            renderer.textures = None;
        }
        return Ok(());
    };

    // SAFETY: the swapchain is live for the duration of Present.
    let device: ID3D11Device = unsafe { swapchain.GetDevice() }?;
    let reset = RESET_REQUESTED.swap(false, Ordering::Relaxed);
    if reset
        || renderer
            .as_ref()
            .is_none_or(|current| current.device != device)
    {
        *renderer = Some(Renderer::new(device)?);
    }

    match renderer.as_mut() {
        Some(renderer) => renderer.draw(swapchain, &header),
        None => Ok(()),
    }
}

fn presents_to_game_window(swapchain: &IDXGISwapChain) -> Result<bool> {
    let game_window = GAME_WINDOW.load(Ordering::Relaxed);
    if game_window == 0 {
        return Ok(false);
    }

    let mut desc = DXGI_SWAP_CHAIN_DESC::default();
    // SAFETY: the out pointer is valid, and the swapchain is live for the duration of Present.
    unsafe { swapchain.GetDesc(&mut desc) }?;
    Ok(desc.OutputWindow.0 == game_window)
}

/// D3D11 objects that live as long as the game's device.
struct Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    rasterizer: ID3D11RasterizerState,
    textures: Option<SharedTextures>,
    /// Texture handles that failed to open. They are not retried until Blish publishes new
    /// ones, instead of failing again every frame.
    unusable: Option<[u64; 2]>,
}

impl Renderer {
    fn new(device: ID3D11Device) -> Result<Self> {
        // SAFETY: the device is live.
        let context = unsafe { device.GetImmediateContext() }?;

        // In each call below the device is live, the descriptor is valid for the call, and
        // `create` supplies a valid out pointer.
        Ok(Self {
            vertex_shader: create(|out| {
                // SAFETY: see above; the bytecode is a complete compiled vertex shader.
                unsafe { device.CreateVertexShader(VERTEX_SHADER, None, Some(out)) }
            })?,
            pixel_shader: create(|out| {
                // SAFETY: see above; the bytecode is a complete compiled pixel shader.
                unsafe { device.CreatePixelShader(PIXEL_SHADER, None, Some(out)) }
            })?,
            sampler: create(|out| {
                // SAFETY: see above.
                unsafe { device.CreateSamplerState(&sampler_desc(), Some(out)) }
            })?,
            blend: create(|out| {
                // SAFETY: see above.
                unsafe { device.CreateBlendState(&blend_desc(), Some(out)) }
            })?,
            rasterizer: create(|out| {
                // SAFETY: see above.
                unsafe { device.CreateRasterizerState(&rasterizer_desc(), Some(out)) }
            })?,
            device,
            context,
            textures: None,
            unusable: None,
        })
    }

    fn draw(&mut self, swapchain: &IDXGISwapChain, header: &Header) -> Result<()> {
        let Some(frame) = self.frame(header)? else {
            return Ok(());
        };

        // SAFETY: the swapchain is live for the duration of Present.
        let backbuffer: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0) }?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the out pointer is valid.
        unsafe { backbuffer.GetDesc(&mut desc) };

        let target = create(|out| {
            // SAFETY: the backbuffer belongs to this device, and the out pointer is valid.
            unsafe {
                self.device
                    .CreateRenderTargetView(&backbuffer, None, Some(out))
            }
        })?;

        let viewport = D3D11_VIEWPORT {
            Width: desc.Width as f32,
            Height: desc.Height as f32,
            MaxDepth: 1.0,
            ..Default::default()
        };

        let context = &self.context;
        // SAFETY: every object bound was created on this context's device and outlives the
        // calls; nothing referencing the backbuffer or Blish's texture is left bound on return.
        unsafe {
            context.IASetInputLayout(None::<&ID3D11InputLayout>);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

            context.VSSetShader(&self.vertex_shader, None);
            context.HSSetShader(None::<&ID3D11HullShader>, None);
            context.DSSetShader(None::<&ID3D11DomainShader>, None);
            context.GSSetShader(None::<&ID3D11GeometryShader>, None);
            context.PSSetShader(&self.pixel_shader, None);
            context.PSSetShaderResources(0, Some(&[Some(frame)]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

            context.RSSetState(&self.rasterizer);
            context.RSSetViewports(Some(&[viewport]));

            context.OMSetBlendState(&self.blend, Some(&[0.0; 4]), u32::MAX);
            context.OMSetDepthStencilState(None::<&ID3D11DepthStencilState>, 0);
            context.OMSetRenderTargets(Some(&[Some(target)]), None);

            context.Draw(3, 0);

            context.PSSetShaderResources(0, Some(&[None]));
            context.OMSetRenderTargets(None, None);
        }

        if !DREW_FRAME.swap(true, Ordering::Relaxed) {
            log::info!("drawing Blish HUD");
        }
        Ok(())
    }

    /// A view of the most recently completed frame, opening Blish's textures if they changed.
    fn frame(&mut self, header: &Header) -> Result<Option<ID3D11ShaderResourceView>> {
        let current = self
            .textures
            .as_ref()
            .is_some_and(|textures| textures.handles == header.textures);

        if !current {
            self.textures = None;
            if self.unusable == Some(header.textures) {
                return Ok(None);
            }

            match SharedTextures::open(&self.device, header.textures) {
                Ok(textures) => {
                    self.unusable = None;
                    self.textures = Some(textures);
                }
                Err(error) => {
                    self.unusable = Some(header.textures);
                    return Err(error);
                }
            }
        }

        Ok(self
            .textures
            .as_ref()
            .map(|textures| textures.views[header.completed_texture()].clone()))
    }
}

/// Views of Blish's two shared render textures.
struct SharedTextures {
    handles: [u64; 2],
    views: [ID3D11ShaderResourceView; 2],
}

impl SharedTextures {
    fn open(device: &ID3D11Device, handles: [u64; 2]) -> Result<Self> {
        let view = |handle: u64| -> Result<ID3D11ShaderResourceView> {
            let texture: ID3D11Texture2D = create(|out| {
                // SAFETY: the handle was published by Blish as a shared texture; a stale or
                // bogus one makes the call fail rather than misbehave.
                unsafe { device.OpenSharedResource(HANDLE(handle as isize), out) }
            })?;
            create(|out| {
                // SAFETY: the texture was just opened on this device, and the out pointer is
                // valid.
                unsafe { device.CreateShaderResourceView(&texture, None, Some(out)) }
            })
        };

        Ok(Self {
            handles,
            views: [view(handles[0])?, view(handles[1])?],
        })
    }
}

/// Runs a D3D11 `Create*` call that fills an out parameter, returning the created object.
fn create<T>(call: impl FnOnce(&mut Option<T>) -> windows::core::Result<()>) -> Result<T> {
    let mut object = None;
    call(&mut object)?;
    created(object)
}

/// Unwraps an object D3D11 reported creating.
fn created<T>(object: Option<T>) -> Result<T> {
    object.ok_or_else(|| Error::Unavailable("D3D11 reported success but returned no object".into()))
}

fn sampler_desc() -> D3D11_SAMPLER_DESC {
    D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        MaxLOD: D3D11_FLOAT32_MAX,
        ..Default::default()
    }
}

/// Standard alpha blending of the overlay over the game.
fn blend_desc() -> D3D11_BLEND_DESC {
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: TRUE,
        SrcBlend: D3D11_BLEND_SRC_ALPHA,
        DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
        BlendOp: D3D11_BLEND_OP_ADD,
        SrcBlendAlpha: D3D11_BLEND_ONE,
        DestBlendAlpha: D3D11_BLEND_ZERO,
        BlendOpAlpha: D3D11_BLEND_OP_ADD,
        RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
    };
    desc
}

/// No culling, so the full-screen triangle draws whatever its winding; no scissor.
fn rasterizer_desc() -> D3D11_RASTERIZER_DESC {
    D3D11_RASTERIZER_DESC {
        FillMode: D3D11_FILL_SOLID,
        CullMode: D3D11_CULL_NONE,
        DepthClipEnable: TRUE,
        ..Default::default()
    }
}
