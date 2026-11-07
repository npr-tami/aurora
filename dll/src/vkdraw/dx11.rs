//! DirectX 11 drawing (GRB.exe), added. Same idea as the Vulkan side: the game's "show this frame"
//! call (IDXGISwapChain::Present, and Present1) is hooked, and the boxes and the menu go into the
//! frame just before it goes out. Plain inline hooks, like the Vulkan one.
//!
//! No shaders and no draw state: ESP rectangles are ClearView calls on the frame (Direct3D 11.1),
//! the menu, toast and command box are copies (CopySubresourceRegion) out of a texture of our own
//! in the frame's own pixel format. The game's pipeline is never touched, so nothing needs saving
//! or restoring.
//!
//! The frame (back buffer) and its view are taken at each present and let go at its end, so no
//! reference of ours ever blocks the game's resize or a new swapchain.
//!
//! COM by hand on windows-sys: a call is a numbered slot in the object's function table. The slot
//! numbers and ids were checked against the Windows SDK 10.0.26100 headers.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use retour::RawDetour;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_OVERLAPPEDWINDOW};
use windows_sys::core::GUID;

use super::*;
use crate::{log, memory};

type Hr = i32;

// Function table slots (IUnknown is 0..=2 everywhere).
const SC_GET_DEVICE: usize = 7;
const SC_PRESENT: usize = 8;
const SC_GET_BUFFER: usize = 9;
const SC_RESIZE_BUFFERS: usize = 13;
const SC1_PRESENT1: usize = 22;
const SC3_RESIZE_BUFFERS1: usize = 39;
const DEV_CREATE_TEXTURE2D: usize = 5;
const DEV_CREATE_RTV: usize = 9;
const DEV_CHECK_FEATURE: usize = 33;
const DEV_GET_CONTEXT: usize = 40;
const CTX_COPY_REGION: usize = 46;
const CTX_UPDATE: usize = 48;
const CTX1_CLEAR_VIEW: usize = 132;
const TEX_GET_DESC: usize = 10;

const IID_TEXTURE2D: GUID = GUID::from_u128(0x6f15aaf2_d208_4e89_9ab4_489535d34f9c);
const IID_DEVICE: GUID = GUID::from_u128(0xdb6f6ddb_ac77_4e88_8253_819df9bbf140);
const IID_CONTEXT1: GUID = GUID::from_u128(0xbb2c6faa_b5fb_4082_8e6b_388b8cfa90e1);
const IID_SWAPCHAIN1: GUID = GUID::from_u128(0x790a45f7_0d42_4876_983a_0a55cfe6f4aa);
const IID_SWAPCHAIN3: GUID = GUID::from_u128(0x94d99bdb_f1f8_4ab0_b236_7da0170edab1);

const PRESENT_TEST: u32 = 1;
const BIND_RENDER_TARGET: u32 = 0x20;
const USAGE_DEFAULT: u32 = 0;
const FEATURE_D3D11_OPTIONS: u32 = 5;
const FMT_RGBA8: u32 = 28;
const FMT_RGBA8_SRGB: u32 = 29;
const FMT_BGRA8: u32 = 87;
const FMT_BGRX8: u32 = 88;
const FMT_BGRA8_SRGB: u32 = 91;
const FMT_BGRX8_SRGB: u32 = 93;
const FMT_RGB10A2: u32 = 24;
const FMT_RGBA16F: u32 = 10;

/// Call slot `$slot` of a COM object: `call!(obj, SLOT, fn(args) -> ret, args...)`.
macro_rules! call {
    ($obj:expr, $slot:expr, fn($($t:ty),*) $(-> $r:ty)?, $($arg:expr),*) => {{
        let o: *mut c_void = $obj;
        let f: unsafe extern "system" fn(*mut c_void $(, $t)*) $(-> $r)? = std::mem::transmute(slot(o, $slot));
        f(o $(, $arg)*)
    }};
}

/// Function `n` of a COM object's table.
unsafe fn slot(obj: *mut c_void, n: usize) -> *const c_void {
    // SAFETY: the caller passes a live COM object; its first word is its table.
    unsafe { *(*(obj as *const *const *const c_void)).add(n) }
}

/// One COM reference of ours, let go on drop.
struct Com(*mut c_void);
// SAFETY: AddRef/Release are thread-safe on D3D11/DXGI objects; the immediate context is only
// Used on the game's present thread, under the DEV lock.
unsafe impl Send for Com {}

impl Com {
    /// Takes over a reference just handed to us (null: None).
    fn new(p: *mut c_void) -> Option<Com> {
        (!p.is_null()).then_some(Com(p))
    }
    fn ptr(&self) -> *mut c_void {
        self.0
    }
    fn query(&self, iid: &GUID) -> Option<Com> {
        let mut out = std::ptr::null_mut();
        // SAFETY: QueryInterface on our live object.
        let hr = unsafe { call!(self.0, 0, fn(*const GUID, *mut *mut c_void) -> Hr, iid, &mut out) };
        if hr >= 0 { Com::new(out) } else { None }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: Release of the one reference this value owns.
        unsafe { call!(self.0, 2, fn() -> u32,) };
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct TexDesc {
    width: u32,
    height: u32,
    mip_levels: u32,
    array_size: u32,
    format: u32,
    sample_count: u32,
    sample_quality: u32,
    usage: u32,
    bind_flags: u32,
    cpu_access: u32,
    misc: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct D3dBox {
    left: u32,
    top: u32,
    front: u32,
    right: u32,
    bottom: u32,
    back: u32,
}

#[repr(C)]
struct ModeDesc {
    width: u32,
    height: u32,
    refresh_num: u32,
    refresh_den: u32,
    format: u32,
    scanline: u32,
    scaling: u32,
}

#[repr(C)]
struct SwapDesc {
    buffer: ModeDesc,
    sample_count: u32,
    sample_quality: u32,
    usage: u32,
    count: u32,
    window: HWND,
    windowed: i32,
    effect: u32,
    flags: u32,
}

/// How a frame's pixels are laid out in memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Px {
    Rgba,
    Bgra,
    Rgb10A2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Fmt {
    px: Px,
    srgb: bool,
}

/// The back-buffer formats we draw into. Others (16-bit float HDR among them) are skipped.
fn fmt_of(format: u32) -> Option<Fmt> {
    let f = |px, srgb| Some(Fmt { px, srgb });
    match format {
        FMT_RGBA8 => f(Px::Rgba, false),
        FMT_RGBA8_SRGB => f(Px::Rgba, true),
        FMT_BGRA8 | FMT_BGRX8 => f(Px::Bgra, false),
        FMT_BGRA8_SRGB | FMT_BGRX8_SRGB => f(Px::Bgra, true),
        FMT_RGB10A2 => f(Px::Rgb10A2, false),
        _ => None,
    }
}

/// Can we draw into this back buffer? Err says why not (logged once).
fn usable(d: &TexDesc) -> Result<Fmt, String> {
    if d.sample_count > 1 {
        return Err(format!("the frame is multisampled ({} samples)", d.sample_count));
    }
    if d.format == FMT_RGBA16F {
        return Err("the frame is 16-bit float (HDR)".into());
    }
    let f = fmt_of(d.format).ok_or_else(|| format!("frame format {} not handled", d.format))?;
    if d.usage != USAGE_DEFAULT {
        return Err(format!("frame usage {} not handled", d.usage));
    }
    Ok(f)
}

/// The menu's RGBA pixels in the frame's layout. The bytes go in as they are (no colour change):
/// the menu looks the same on sRGB and plain frames, as on Vulkan.
fn convert(px: Px, rgba: &[u8]) -> Vec<u8> {
    match px {
        Px::Rgba => rgba.to_vec(),
        Px::Bgra => rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
        Px::Rgb10A2 => rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let ten = |v: u8| (u32::from(v) * 1023 + 127) / 255;
                let a = (u32::from(p[3]) * 3 + 127) / 255;
                (ten(p[0]) | ten(p[1]) << 10 | ten(p[2]) << 20 | a << 30).to_le_bytes()
            })
            .collect(),
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// ClearView's colour for an ESP colour: an sRGB frame stores what it is given converted to sRGB,
/// so it gets the linear value (else the boxes come out paler than picked).
fn clear_colour(f: Fmt, c: [f32; 4]) -> [f32; 4] {
    if f.srgb { [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2]), c[3]] } else { c }
}

/// One panel of a picture placed at (x, y), clipped to the picture (fw x fh) and to the screen (sw
/// x sh): where it lands and the part of our texture copied. None: nothing shows.
fn clip(x: i32, y: i32, p: [u32; 4], (fw, fh): (u32, u32), (sw, sh): (i32, i32)) -> Option<((u32, u32), D3dBox)> {
    let (p2, p3) = (p[2].min(fw) as i32, p[3].min(fh) as i32);
    let (x0, y0) = ((x + p[0] as i32).max(0), (y + p[1] as i32).max(0));
    let (x1, y1) = ((x + p2).min(sw), (y + p3).min(sh));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (sx, sy) = ((x0 - x) as u32, (y0 - y) as u32);
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    Some(((x0 as u32, y0 as u32), D3dBox { left: sx, top: sy, front: 0, right: sx + w, bottom: sy + h, back: 1 }))
}

/// Where a function's first instruction jumps, if it is a jump (an overlay's hook).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Jump {
    /// Jmp rel32: the target itself.
    To(u64),
    /// Jmp [rip+disp32]: the address holding the target.
    Via(u64),
}

fn decode_jump(at: u64, b: &[u8; 14]) -> Option<Jump> {
    let rel = |i: usize| i64::from(i32::from_le_bytes(b[i..i + 4].try_into().unwrap()));
    match b {
        [0xe9, ..] => Some(Jump::To(at.wrapping_add(5).wrapping_add_signed(rel(1)))),
        [0xff, 0x25, ..] => Some(Jump::Via(at.wrapping_add(6).wrapping_add_signed(rel(2)))),
        // Mov rax, imm64; jmp rax
        [0x48, 0xb8, _, _, _, _, _, _, _, _, 0xff, 0xe0, _, _] =>Some(Jump::To(u64::from_le_bytes(b[2..10].try_into().unwrap()))),
        _ => None,
    }
}

/// The module a hooked function's first jump leads to (two hops at most: some hooks jump to a small
/// stub first).
fn jump_module(at: usize) -> Option<String> {
    let mut at = at as u64;
    for _ in 0..2 {
        let mut b = [0u8; 14];
        if safe_read(at, &mut b) != 14 {
            return None;
        }
        at = match decode_jump(at, &b)? {
            Jump::To(t) => t,
            Jump::Via(p) => rd_u64(p)?,
        };
        if let Some(m) = module_name(at as usize) {
            return Some(m);
        }
    }
    Some(format!("{at:#x}"))
}

/// Hook.txt's line (the Launchpad reads its first words): who had Present before us.
fn plain_place(present: usize) -> String {
    match jump_module(present) {
        Some(m) if m.to_lowercase().starts_with("gameoverlayrenderer") => format!("Steam overlay ({m})"),
        Some(m) => format!("Other layer ({m})"),
        None => format!("DXGI ({})", module_name(present).unwrap_or_else(|| "dxgi.dll".into())),
    }
}

type PresentFn = unsafe extern "system" fn(*mut c_void, u32, u32) -> Hr;
type Present1Fn = unsafe extern "system" fn(*mut c_void, u32, u32, *const c_void) -> Hr;
type ResizeFn = unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32, u32) -> Hr;
type Resize1Fn = unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32, u32, *const u32, *const *mut c_void) -> Hr;
type CreateFn = unsafe extern "system" fn(
    *mut c_void,
    u32,
    *mut c_void,
    u32,
    *const u32,
    u32,
    u32,
    *const SwapDesc,
    *mut *mut c_void,
    *mut *mut c_void,
    *mut u32,
    *mut *mut c_void,
) -> Hr;

static ORIG_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIG_PRESENT1: AtomicUsize = AtomicUsize::new(0);
static ORIG_RESIZE: AtomicUsize = AtomicUsize::new(0);
static ORIG_RESIZE1: AtomicUsize = AtomicUsize::new(0);
/// The thread inside our outermost present hook (0: none). Dxgi may run one present through the
/// other; the inner one then only passes the call on, so a frame is drawn once.
static OUTER: AtomicU32 = AtomicU32::new(0);

/// Our textures on the game's device.
struct Dev {
    device: Com,
    ctx: Com,
    /// The Direct3D 11.1 context, for ClearView. None: no ESP.
    ctx1: Option<Com>,
    clear_view: bool,
    menu: [Option<MenuTex>; PIC_SLOTS],
}

/// A picture slot's texture, in the frame's format, and the picture it holds.
struct MenuTex {
    tex: Com,
    format: u32,
    generation: u64,
}

static DEV: Mutex<Option<Dev>> = Mutex::new(None);
/// Things said once in the log (unusable frame, no ClearView...).
static SAID: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn once(msg: String) {
    if let Ok(mut s) = SAID.lock()
        && !s.contains(&msg)
    {
        log(&msg);
        s.push(msg);
    }
}

impl Dev {
    fn new(device: Com) -> Result<Dev, String> {
        let mut ctx = std::ptr::null_mut();
        // SAFETY: the game's live device; GetImmediateContext adds a reference we own.
        unsafe { call!(device.ptr(), DEV_GET_CONTEXT, fn(*mut *mut c_void), &mut ctx) };
        let ctx = Com::new(ctx).ok_or("the device has no immediate context")?;
        let ctx1 = ctx.query(&IID_CONTEXT1);
        // D3D11_FEATURE_DATA_D3D11_OPTIONS: 14 BOOLs, ClearView is the 5th.
        let mut opts = [0i32; 14];
        // SAFETY: the struct is as big as we say.
        let hr = unsafe {
            call!(device.ptr(), DEV_CHECK_FEATURE, fn(u32, *mut c_void, u32) -> Hr, FEATURE_D3D11_OPTIONS, opts.as_mut_ptr() as *mut c_void, std::mem::size_of_val(&opts) as u32)
        };
        let clear_view = hr >= 0 && opts[4] != 0;
        log(&format!("dx11: ready to draw (11.1 context {}, ClearView {clear_view})", ctx1.is_some()));
        Ok(Dev { device, ctx, ctx1, clear_view, menu: [None, None, None, None] })
    }

    /// A picture slot's texture in this format, made once (again if the format changes).
    fn menu_tex(&mut self, slot: usize, format: u32) -> Result<&mut MenuTex, String> {
        if self.menu[slot].as_ref().is_none_or(|m| m.format != format) {
            self.menu[slot] = None;
            let (width, height) = slot_size(slot);
            let desc = TexDesc {
                width,
                height,
                mip_levels: 1,
                array_size: 1,
                format,
                sample_count: 1,
                sample_quality: 0,
                usage: USAGE_DEFAULT,
                // Only copied from, never read by a shader.
                bind_flags: 0,
                cpu_access: 0,
                misc: 0,
            };
            let mut t = std::ptr::null_mut();
            // SAFETY: CreateTexture2D on the game's device with our own description.
            let hr = unsafe {
                call!(self.device.ptr(), DEV_CREATE_TEXTURE2D, fn(*const TexDesc, *const c_void, *mut *mut c_void) -> Hr, &desc, std::ptr::null(), &mut t)
            };
            let tex = Com::new(t).filter(|_| hr >= 0).ok_or_else(|| format!("menu texture: {hr:#x}"))?;
            self.menu[slot] = Some(MenuTex { tex, format, generation: 0 });
            log("dx11: menu texture ready");
        }
        Ok(self.menu[slot].as_mut().unwrap())
    }
}

/// The game's frame being presented: buffer 0 (with D3D11 that is always the current one).
unsafe fn back_buffer(swap: *mut c_void) -> Option<(Com, TexDesc)> {
    let mut out = std::ptr::null_mut();
    // SAFETY: the game's live swapchain; GetBuffer adds a reference we own.
    let hr = unsafe { call!(swap, SC_GET_BUFFER, fn(u32, *const GUID, *mut *mut c_void) -> Hr, 0, &IID_TEXTURE2D, &mut out) };
    let bb = Com::new(out).filter(|_| hr >= 0)?;
    let mut d = TexDesc::default();
    // SAFETY: GetDesc fills our struct.
    unsafe { call!(bb.ptr(), TEX_GET_DESC, fn(*mut TexDesc), &mut d) };
    Some((bb, d))
}

/// Draw into the frame being presented. Ok(true): something was drawn.
unsafe fn draw(swap: *mut c_void) -> Result<bool, String> {
    let cfg = config();
    let pics = PICS.lock().map(|m| m.clone()).unwrap_or_default();
    let menu = pics.iter().any(|p| p.is_some());
    if !drawing(&cfg) && !menu {
        return Ok(false);
    }
    // The lock comes before the frame, so the frame (declared later) is let go before the lock: a
    // resize that waits on the lock then never meets our reference. Lock order is DEV, then DXGI's
    // own, as in the resize hooks.
    let mut guard = DEV.lock().map_err(|_| "dx11 lock poisoned")?;
    // SAFETY: the game's swapchain, inside its own present call.
    let Some((bb, desc)) = (unsafe { back_buffer(swap) }) else {
        once("dx11: the frame could not be read (GetBuffer)".into());
        return Ok(false);
    };
    LAST_W.store(desc.width, Ordering::Relaxed);
    LAST_H.store(desc.height, Ordering::Relaxed);
    let fmt = match usable(&desc) {
        Ok(f) => f,
        Err(why) => {
            once(format!("dx11: not drawing: {why}"));
            NO_SAFE.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        }
    };
    let (w, h) = (desc.width as i32, desc.height as i32);
    // The picked target is drawn white, as on Vulkan.
    let cfg = Config { target: crate::entities::target_body(), ..cfg };
    let rects = with_circle(if cfg.on { boxes(&cfg, w, h) } else { Default::default() }, w, h);
    if rects.iter().all(|r| r.is_empty()) && !menu {
        return Ok(false);
    }
    let mut out = std::ptr::null_mut();
    // SAFETY: as above; GetDevice adds a reference we own.
    let hr = unsafe { call!(swap, SC_GET_DEVICE, fn(*const GUID, *mut *mut c_void) -> Hr, &IID_DEVICE, &mut out) };
    let Some(device) = Com::new(out).filter(|_| hr >= 0) else {
        once(format!("dx11: the frame's device is not a D3D11 one ({hr:#x})"));
        return Ok(false);
    };
    if guard.as_ref().is_some_and(|d| d.device.ptr() != device.ptr()) {
        // Logged once: two devices presenting (an overlay's own swapchain) would say it every other
        // frame.
        once("dx11: the game made a new device, starting over".into());
        *guard = None;
    }
    if guard.is_none() {
        *guard = Some(Dev::new(device)?);
    }
    let dev = guard.as_mut().unwrap();
    let mut drew = false;
    if rects.iter().any(|r| !r.is_empty()) {
        if desc.bind_flags & BIND_RENDER_TARGET == 0 {
            once("dx11: the frame is not a render target, no ESP".into());
        } else if let (Some(ctx1), true) = (&dev.ctx1, dev.clear_view) {
            let mut rtv = std::ptr::null_mut();
            // SAFETY: a view of the frame, made with the frame's own format; we own it.
            let hr = unsafe {
                call!(dev.device.ptr(), DEV_CREATE_RTV, fn(*mut c_void, *const c_void, *mut *mut c_void) -> Hr, bb.ptr(), std::ptr::null(), &mut rtv)
            };
            match Com::new(rtv).filter(|_| hr >= 0) {
                Some(rtv) => {
                    let want = colors(&cfg.style);
                    // Shadows first, so the colours land on top.
                    for c in DRAW_ORDER {
                        if rects[c].is_empty() {
                            continue;
                        }
                        let rs: Vec<RECT> = rects[c].iter().map(|r| RECT { left: r[0], top: r[1], right: r[2], bottom: r[3] }).collect();
                        let col = clear_colour(fmt, want[c]);
                        // SAFETY: our view, the colour and the rectangles live across the call.
                        unsafe {
                            call!(ctx1.ptr(), CTX1_CLEAR_VIEW, fn(*mut c_void, *const f32, *const RECT, u32), rtv.ptr(), col.as_ptr(), rs.as_ptr(), rs.len() as u32)
                        };
                    }
                    drew = true;
                }
                None => once(format!("dx11: no view of the frame ({hr:#x}), no ESP")),
            }
        } else {
            once("dx11: no ClearView (needs Direct3D 11.1), no ESP".into());
        }
    }
    let ctx = dev.ctx.ptr();
    for (slot, pic) in pics.iter().enumerate() {
        let Some(pic) = pic.as_deref() else { continue };
        let f = &pic.frame;
        if f.w == 0 || f.h == 0 || f.w > slot_size(slot).0 || f.h > slot_size(slot).1 {
            continue;
        }
        let Some(rgba) = f.rgba.get(..(f.w * f.h * 4) as usize) else { continue };
        let mt = match dev.menu_tex(slot, desc.format) {
            Ok(m) => m,
            Err(e) => {
                if !MENU_FAILED.swap(true, Ordering::Relaxed) {
                    log(&format!("dx11: no menu: {e}"));
                }
                continue;
            }
        };
        if mt.generation != pic.generation {
            // A new picture: its pixels into our texture.
            let bytes = convert(fmt.px, rgba);
            let bx = D3dBox { left: 0, top: 0, front: 0, right: f.w, bottom: f.h, back: 1 };
            // SAFETY: our texture; the box lies inside it and the bytes hold f.w x f.h pixels.
            unsafe {
                call!(ctx, CTX_UPDATE, fn(*mut c_void, u32, *const D3dBox, *const c_void, u32, u32), mt.tex.ptr(), 0, &bx, bytes.as_ptr() as *const c_void, f.w * 4, 0)
            };
            mt.generation = pic.generation;
        }
        // Only the panels go into the frame, clipped to the screen.
        for p in &f.panels {
            if let Some(((dx, dy), bx)) = clip(pic.x, pic.y, *p, (f.w, f.h), (w, h)) {
                // SAFETY: same format on both sides; the box and the spot are inside each.
                unsafe {
                    call!(ctx, CTX_COPY_REGION, fn(*mut c_void, u32, u32, u32, u32, *mut c_void, u32, *const D3dBox), bb.ptr(), 0, dx, dy, 0, mt.tex.ptr(), 0, &bx)
                };
                drew = true;
            }
        }
    }
    Ok(drew)
}

/// Every present (Present and Present1): heartbeat, then draw, never twice for one frame.
fn on_present(swap: *mut c_void, flags: u32) {
    let n = PRESENTS.fetch_add(1, Ordering::Relaxed);
    if n.is_multiple_of(300) {
        heartbeat("dx11", n);
    }
    // A test present shows nothing.
    if swap.is_null() || flags & PRESENT_TEST != 0 || BROKEN.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: the game's swapchain, inside its present call.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { draw(swap) })) {
        Ok(Ok(true)) => {
            DRAWN.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Ok(false)) => {}
        Ok(Err(e)) => {
            BROKEN.store(true, Ordering::SeqCst);
            log(&format!("dx11: stopped drawing: {e}"));
        }
        Err(_) => {
            BROKEN.store(true, Ordering::SeqCst);
            log("dx11: stopped drawing after a panic");
        }
    }
}

/// Runs `orig` with our drawing first, unless this thread is already inside a present hook.
fn present_with(swap: *mut c_void, flags: u32, orig: impl FnOnce() -> Hr) -> Hr {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    // SAFETY: plain thread id query.
    let me = unsafe { GetCurrentThreadId() };
    let outer = OUTER.compare_exchange(0, me, Ordering::SeqCst, Ordering::SeqCst).is_ok();
    if outer || OUTER.load(Ordering::SeqCst) != me {
        on_present(swap, flags);
    }
    let r = orig();
    if outer {
        OUTER.store(0, Ordering::SeqCst);
    }
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

unsafe extern "system" fn hk_present(swap: *mut c_void, sync: u32, flags: u32) -> Hr {
    present_with(swap, flags, || {
        // SAFETY: the trampoline runs the original Present with the game's arguments.
        unsafe {
            let orig: PresentFn = std::mem::transmute(ORIG_PRESENT.load(Ordering::SeqCst));
            orig(swap, sync, flags)
        }
    })
}

unsafe extern "system" fn hk_present1(swap: *mut c_void, sync: u32, flags: u32, params: *const c_void) -> Hr {
    present_with(swap, flags, || {
        // SAFETY: as above, for Present1.
        unsafe {
            let orig: Present1Fn = std::mem::transmute(ORIG_PRESENT1.load(Ordering::SeqCst));
            orig(swap, sync, flags, params)
        }
    })
}

/// Before the game resizes its frames (alt-tab, resolution, fullscreen): it fails while anyone
/// holds a frame. We hold none between presents; taking the lock waits for a draw on another thread
/// to let its frame go. The size is read again at the next present.
fn before_resize(w: u32, h: u32) {
    drop(DEV.lock());
    LAST_W.store(0, Ordering::Relaxed);
    LAST_H.store(0, Ordering::Relaxed);
    log(&format!("dx11: the game resizes its frames ({w}x{h})"));
}

unsafe extern "system" fn hk_resize(swap: *mut c_void, count: u32, w: u32, h: u32, format: u32, flags: u32) -> Hr {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    before_resize(w, h);
    // SAFETY: the trampoline runs the original ResizeBuffers with the game's arguments.
    let r = unsafe {
        let orig: ResizeFn = std::mem::transmute(ORIG_RESIZE.load(Ordering::SeqCst));
        orig(swap, count, w, h, format, flags)
    };
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn hk_resize1(
    swap: *mut c_void,
    count: u32,
    w: u32,
    h: u32,
    format: u32,
    flags: u32,
    mask: *const u32,
    queues: *const *mut c_void,
) -> Hr {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    before_resize(w, h);
    // SAFETY: as above, for ResizeBuffers1.
    let r = unsafe {
        let orig: Resize1Fn = std::mem::transmute(ORIG_RESIZE1.load(Ordering::SeqCst));
        orig(swap, count, w, h, format, flags, mask, queues)
    };
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

/// A hidden window of our own for the throwaway swapchain; gone on drop. Windows' own "STATIC"
/// class, so there is no class to register and remove.
struct ProbeWindow(HWND);

impl ProbeWindow {
    fn new() -> Result<ProbeWindow, String> {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: plain user32 call; the class name lives across it. Never shown.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() { Err("the probe window could not be made".into()) } else { Ok(ProbeWindow(hwnd)) }
    }
}

impl Drop for ProbeWindow {
    fn drop(&mut self) {
        // SAFETY: our own window, made on this thread.
        unsafe { DestroyWindow(self.0) };
    }
}

/// Present, ResizeBuffers, Present1 and ResizeBuffers1 as a throwaway device and swapchain see them
/// (0: that one is missing). All of it is let go before returning.
fn probe() -> Result<[usize; 4], String> {
    // SAFETY: d3d11.dll is the game's own (it imports it); everything made here is ours and
    // Released below.
    unsafe {
        let d3d11 = GetModuleHandleA(c"d3d11.dll".as_ptr() as _);
        if d3d11.is_null() {
            return Err("d3d11.dll is not loaded".into());
        }
        let create = GetProcAddress(d3d11, c"D3D11CreateDeviceAndSwapChain".as_ptr() as _).ok_or("D3D11CreateDeviceAndSwapChain missing")?;
        let create: CreateFn = std::mem::transmute(create);
        let window = ProbeWindow::new()?;
        let desc = SwapDesc {
            buffer: ModeDesc { width: 64, height: 64, refresh_num: 60, refresh_den: 1, format: FMT_RGBA8, scanline: 0, scaling: 0 },
            sample_count: 1,
            sample_quality: 0,
            usage: 0x20,
            count: 1,
            window: window.0,
            windowed: 1,
            effect: 0,
            flags: 0,
        };
        // Feature levels 11_0 and 10_0; hardware first, then WARP (software).
        let levels = [0xb000u32, 0xa000];
        let mut last = 0;
        for driver in [1u32, 5] {
            let (mut sc, mut dev, mut ctx) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            let hr = create(
                std::ptr::null_mut(),
                driver,
                std::ptr::null_mut(),
                0,
                levels.as_ptr(),
                levels.len() as u32,
                7,
                &desc,
                &mut sc,
                &mut dev,
                std::ptr::null_mut(),
                &mut ctx,
            );
            let (sc, _dev, _ctx) = (Com::new(sc), Com::new(dev), Com::new(ctx));
            if hr < 0 {
                last = hr;
                continue;
            }
            let sc = sc.ok_or("no swapchain made")?;
            let present = slot(sc.ptr(), SC_PRESENT) as usize;
            let resize = slot(sc.ptr(), SC_RESIZE_BUFFERS) as usize;
            let present1 = sc.query(&IID_SWAPCHAIN1).map_or(0, |s| slot(s.ptr(), SC1_PRESENT1) as usize);
            let resize1 = sc.query(&IID_SWAPCHAIN3).map_or(0, |s| slot(s.ptr(), SC3_RESIZE_BUFFERS1) as usize);
            return Ok([present, resize, present1, resize1]);
        }
        Err(format!("D3D11CreateDeviceAndSwapChain failed ({last:#x})"))
    }
}

/// Make a throwaway device to learn where Present lives (in dxgi.dll; an overlay may already jump
/// out of it), then hook it.
pub(super) fn install() -> Result<Hooks, String> {
    let exe = memory::find_module("").ok_or("exe not found")?;
    EXE.store(exe.base, Ordering::SeqCst);
    if hook_at() == HookAt::Driver {
        log("dx11: the GPU driver hook setting is for Vulkan only, hooking as auto");
    }
    let [present, resize, present1, resize1] = probe()?;
    if present == 0 {
        return Err("DXGI present call not found".into());
    }
    let at = |a: usize| if a == 0 { "none".to_string() } else { module_of(a) };
    log(&format!("dx11: present at {}, resize at {}, present1 at {}, resize1 at {}", at(present), at(resize), at(present1), at(resize1)));
    // Read before our hook goes in: whose jump is at the start now.
    let place = plain_place(present);
    let hooks = detour([
        (present, hk_present as *const (), &ORIG_PRESENT),
        (resize, hk_resize as *const (), &ORIG_RESIZE),
        (present1, hk_present1 as *const (), &ORIG_PRESENT1),
        (resize1, hk_resize1 as *const (), &ORIG_RESIZE1),
    ])?;
    hook_note(&place);
    log(&format!("dx11: hooked ({place})"));
    Ok(hooks)
}

/// Detour each function found (0: skipped); keep each one's first bytes with our jump in.
fn detour(list: [(usize, *const (), &AtomicUsize); 4]) -> Result<Hooks, String> {
    let mut hooks = Vec::new();
    let mut targets = Vec::new();
    // SAFETY: targets are live dxgi functions with exactly these signatures; each detour has
    // Its trampoline address stored before it is enabled.
    unsafe {
        for (target, hook, orig) in list {
            if target == 0 || targets.contains(&target) {
                continue;
            }
            let d = RawDetour::new(target as *const (), hook).map_err(|e| format!("hook {}: {e}", module_of(target)))?;
            orig.store(d.trampoline() as *const () as usize, Ordering::SeqCst);
            hooks.push(d);
            targets.push(target);
        }
        // If one fails to go in, the ones already in come out again, and none is dropped: retour's
        // drop does not take a hook out in a release build, and a thread may still be inside a
        // trampoline. A few bytes stay leaked.
        if let Some((n, e)) = hooks.iter().enumerate().find_map(|(i, d)| d.enable().err().map(|e| (i, e))) {
            for d in &hooks[..n] {
                let _ = d.disable();
            }
            std::mem::forget(hooks);
            return Err(format!("enable hook: {e}"));
        }
    }
    let bytes = targets
        .into_iter()
        .map(|at| {
            let mut b = [0u8; 16];
            safe_read(at as u64, &mut b);
            (at, b)
        })
        .collect();
    Ok(Hooks(hooks, bytes))
}

/// At unload, after the hooks are off and no thread is in them: let go of the device, its context
/// and our textures.
pub(super) fn release() {
    // A poisoned lock still holds them: let go anyway.
    *DEV.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_of(FMT_RGBA8), Some(Fmt { px: Px::Rgba, srgb: false }));
        assert_eq!(fmt_of(FMT_BGRA8_SRGB), Some(Fmt { px: Px::Bgra, srgb: true }));
        assert_eq!(fmt_of(FMT_BGRX8), Some(Fmt { px: Px::Bgra, srgb: false }));
        assert_eq!(fmt_of(FMT_RGB10A2), Some(Fmt { px: Px::Rgb10A2, srgb: false }));
        assert_eq!(fmt_of(FMT_RGBA16F), None);
        assert_eq!(fmt_of(27), None, "typeless is not drawn into");
    }

    #[test]
    fn usable_frames() {
        let d = TexDesc { format: FMT_BGRA8, sample_count: 1, bind_flags: BIND_RENDER_TARGET, ..Default::default() };
        assert!(usable(&d).is_ok());
        assert!(usable(&TexDesc { sample_count: 4, ..d }).unwrap_err().contains("multisampled"));
        assert!(usable(&TexDesc { format: FMT_RGBA16F, ..d }).unwrap_err().contains("HDR"));
        assert!(usable(&TexDesc { format: 2, ..d }).is_err());
        assert!(usable(&TexDesc { usage: 1, ..d }).is_err());
    }

    #[test]
    fn pixel_conversions() {
        let px = [10u8, 20, 30, 255, 255, 0, 128, 0];
        assert_eq!(convert(Px::Rgba, &px), px.to_vec());
        assert_eq!(convert(Px::Bgra, &px), vec![30, 20, 10, 255, 128, 0, 255, 0]);
        let packed = convert(Px::Rgb10A2, &px);
        let a = u32::from_le_bytes(packed[0..4].try_into().unwrap());
        assert_eq!(a & 0x3ff, (10 * 1023 + 127) / 255);
        assert_eq!(a >> 10 & 0x3ff, (20 * 1023 + 127) / 255);
        assert_eq!(a >> 20 & 0x3ff, (30 * 1023 + 127) / 255);
        assert_eq!(a >> 30, 3);
        let b = u32::from_le_bytes(packed[4..8].try_into().unwrap());
        assert_eq!(b & 0x3ff, 1023);
        assert_eq!(b >> 10 & 0x3ff, 0);
        assert_eq!(b >> 30, 0);
    }

    #[test]
    fn srgb_colours() {
        let plain = Fmt { px: Px::Rgba, srgb: false };
        let srgb = Fmt { px: Px::Rgba, srgb: true };
        assert_eq!(clear_colour(plain, [0.5, 0.2, 1.0, 1.0]), [0.5, 0.2, 1.0, 1.0]);
        let c = clear_colour(srgb, [0.5, 0.0, 1.0, 1.0]);
        assert!((c[0] - 0.214).abs() < 0.001, "{c:?}");
        assert_eq!(c[1], 0.0);
        assert!((c[2] - 1.0).abs() < 1e-6);
        assert_eq!(c[3], 1.0);
    }

    #[test]
    fn clipping() {
        // Fully on screen.
        assert_eq!(clip(100, 50, [10, 10, 60, 40], (200, 100), (1920, 1080)), Some(((110, 60), D3dBox { left: 10, top: 10, front: 0, right: 60, bottom: 40, back: 1 })));
        // Hanging off the left and top.
        assert_eq!(clip(-20, -5, [0, 0, 50, 30], (50, 30), (1920, 1080)), Some(((0, 0), D3dBox { left: 20, top: 5, front: 0, right: 50, bottom: 30, back: 1 })));
        // Hanging off the right and bottom.
        assert_eq!(clip(1900, 1070, [0, 0, 50, 30], (50, 30), (1920, 1080)), Some(((1900, 1070), D3dBox { left: 0, top: 0, front: 0, right: 20, bottom: 10, back: 1 })));
        // A panel larger than its picture is cut to the picture.
        assert_eq!(clip(0, 0, [0, 0, 500, 500], (100, 80), (1920, 1080)), Some(((0, 0), D3dBox { left: 0, top: 0, front: 0, right: 100, bottom: 80, back: 1 })));
        // Off screen.
        assert_eq!(clip(2000, 0, [0, 0, 50, 30], (50, 30), (1920, 1080)), None);
        assert_eq!(clip(-100, 0, [0, 0, 50, 30], (50, 30), (1920, 1080)), None);
    }

    #[test]
    fn jumps() {
        let mut b = [0u8; 14];
        b[0] = 0xe9;
        b[1..5].copy_from_slice(&0x100i32.to_le_bytes());
        assert_eq!(decode_jump(0x1000, &b), Some(Jump::To(0x1105)));
        b[1..5].copy_from_slice(&(-0x20i32).to_le_bytes());
        assert_eq!(decode_jump(0x1000, &b), Some(Jump::To(0x0fe5)));
        let mut b = [0u8; 14];
        b[0] = 0xff;
        b[1] = 0x25;
        assert_eq!(decode_jump(0x2000, &b), Some(Jump::Via(0x2006)));
        let mut b = [0u8; 14];
        b[0] = 0x48;
        b[1] = 0xb8;
        b[2..10].copy_from_slice(&0x7ff6_1234_5678u64.to_le_bytes());
        b[10] = 0xff;
        b[11] = 0xe0;
        assert_eq!(decode_jump(0, &b), Some(Jump::To(0x7ff6_1234_5678)));
        let b = [0x48, 0x89, 0x5c, 0x24, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_jump(0, &b), None);
    }
}
