//! The Vulkan drawing (GRB_vulkan.exe): vkQueuePresentKHR hooked, boxes and the menu blitted into
//! the frame. Moved here from vkdraw.rs without logic changes.

use std::ffi::{CStr, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ash::vk::{self, Handle};
use retour::RawDetour;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, IsWindow, IsWindowVisible};

use super::*;
use crate::{log, memory};

type PresentFn = unsafe extern "system" fn(vk::Queue, *const vk::PresentInfoKHR) -> vk::Result;
type AcquireFn = unsafe extern "system" fn(vk::Device, vk::SwapchainKHR, u64, vk::Semaphore, vk::Fence, *mut u32) -> vk::Result;
type CreateSwapchainFn = unsafe extern "system" fn(
    vk::Device,
    *const vk::SwapchainCreateInfoKHR<'static>,
    *const vk::AllocationCallbacks<'static>,
    *mut vk::SwapchainKHR,
) -> vk::Result;

/// Frames in flight we keep our own command buffers for.
const SLOTS: usize = 4;

static ORIG_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIG_ACQUIRE: AtomicUsize = AtomicUsize::new(0);
static ORIG_CREATE: AtomicUsize = AtomicUsize::new(0);

/// The game's device and swapchain, from its acquire calls.
pub(super) static DEVICE: AtomicU64 = AtomicU64::new(0);
/// Swapchain sizes seen at creation (only when the game makes one after we hooked).
static SWAP_SIZES: Mutex<Vec<(u64, u32, u32)>> = Mutex::new(Vec::new());
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Bumped whenever the game makes a swapchain: a new one can reuse an old handle value, so the
/// cached screen images must be fetched again.
static SWAP_GEN: AtomicU64 = AtomicU64::new(0);
/// Swapchains seen made with the transfer flag (the game's or ours added). Only these are drawn
/// into: the game's own lacks it, and copying into such a frame is not allowed and the likely cause
/// of the GPU resets.
static SAFE_SWAPS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// Swapchains we asked the game to remake (present said 'suboptimal', then 'out of date'): handle,
/// when we first asked, and how many times.
static NUDGED: Mutex<Vec<(u64, Instant, u8)>> = Mutex::new(Vec::new());

/// Said once in the log that no window size was found.
static NO_SIZE_SAID: AtomicBool = AtomicBool::new(false);

struct Gpu {
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    cmds: Vec<vk::CommandBuffer>,
    fences: Vec<vk::Fence>,
    sems: Vec<vk::Semaphore>,
    colors: Vec<(vk::Image, vk::DeviceMemory)>,
    /// The colours the 1x1 images hold now (None: not filled yet).
    colors_now: Option<[[f32; 4]; COLOURS]>,
    /// Made the first time the menu (or a toast) shows.
    menu: [Option<MenuGpu>; PIC_SLOTS],
    get_images: vk::PFN_vkGetSwapchainImagesKHR,
    swapchain: vk::SwapchainKHR,
    swap_gen: u64,
    images: Vec<vk::Image>,
    frame: usize,
}

// SAFETY: Vulkan handles are plain ids; all use goes through the GPU lock.
unsafe impl Send for Gpu {}
static GPU: Mutex<Option<Gpu>> = Mutex::new(None);

/// The hook's place in plain words: `Steam overlay (SteamOverlayVulkanLayer64.dll)`.
fn plain_place(addr: usize) -> String {
    let Some(name) = module_name(addr) else { return format!("Other layer ({addr:#x})") };
    let low = name.to_lowercase();
    let kind = if low.starts_with("steamoverlayvulkanlayer") {
        "Steam overlay"
    } else if ["amdvlk", "amd_vulkan", "nvoglv", "igvk"].iter().any(|d| low.starts_with(d)) {
        "GPU driver"
    } else {
        "Other layer"
    };
    format!("{kind} ({name})")
}

/// At unload, after the hooks are off and no thread is in them: free our Vulkan objects.
pub(super) fn release() {
    if let Some(gpu) = GPU.lock().ok().and_then(|mut g| g.take()) {
        // SAFETY: nothing else uses these objects any more (hooks off, IN_HOOK 0).
        unsafe { destroy(gpu) };
    }
}

unsafe fn destroy(gpu: Gpu) {
    let d = &gpu.device;
    unsafe {
        let _ = d.wait_for_fences(&gpu.fences, true, 1_000_000_000);
        for &f in &gpu.fences {
            d.destroy_fence(f, None);
        }
        for &s in &gpu.sems {
            d.destroy_semaphore(s, None);
        }
        d.destroy_command_pool(gpu.pool, None);
        for &(i, m) in &gpu.colors {
            d.destroy_image(i, None);
            d.free_memory(m, None);
        }
        for mg in gpu.menu.into_iter().flatten() {
            d.destroy_image(mg.image, None);
            d.free_memory(mg.image_mem, None);
            d.destroy_buffer(mg.buf, None);
            d.free_memory(mg.buf_mem, None);
        }
    }
}

/// Set (Some) or unset (None) an environment variable of this process.
fn set_env(name: &str, value: Option<&std::ffi::OsStr>) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Environment::SetEnvironmentVariableW;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let n = wide(std::ffi::OsStr::new(name));
    let v = value.map(wide);
    // SAFETY: both strings are NUL-terminated and alive for the call; null unsets.
    unsafe { SetEnvironmentVariableW(n.as_ptr(), v.as_ref().map_or(std::ptr::null(), |v| v.as_ptr())) };
}

/// The GPU driver's own functions, past every layer (Steam overlay, OBS, RTSS, ReShade): the loader
/// reads these at each vkCreateInstance, so the throwaway instance gets no layers. They go back
/// exactly as they were straight after. None: still a layer.
fn probe_driver() -> Option<(usize, usize, usize)> {
    const VARS: [(&str, &str); 2] = [("VK_LOADER_LAYERS_DISABLE", "~implicit~"), ("DISABLE_VK_LAYER_VALVE_steam_overlay_1", "1")];
    let old: Vec<_> = VARS.iter().map(|(n, _)| std::env::var_os(n)).collect();
    for (n, v) in VARS {
        set_env(n, Some(std::ffi::OsStr::new(v)));
    }
    let r = probe();
    for ((n, _), o) in VARS.iter().zip(&old) {
        set_env(n, o.as_deref());
    }
    let r = r.inspect_err(|e| log(&format!("vkdraw: driver probe failed ({e}), hooking as auto"))).ok()?;
    log(&format!(
        "vkdraw: driver probe: present at {}, acquire at {}, create at {}",
        module_of(r.0),
        module_of(r.1),
        module_of(r.2)
    ));
    if module_name(r.0).is_none_or(|m| m.to_lowercase().contains("layer")) || r.0 == 0 || r.1 == 0 {
        log("vkdraw: driver probe still ended in a layer, hooking as auto");
        return None;
    }
    Some(r)
}

/// Make a throwaway Vulkan device to learn where the present call lives (the first function in the
/// chain, a Steam layer on the PC), then hook it.
pub(super) fn install() -> Result<Hooks, String> {
    let exe = memory::find_module("").ok_or("exe not found")?;
    EXE.store(exe.base, Ordering::SeqCst);
    let found = match hook_at() {
        HookAt::Driver => probe_driver(),
        _ => None,
    };
    let (present, acquire, create) = match found {
        Some(r) => r,
        None => probe()?,
    };
    if present == 0 || acquire == 0 {
        return Err("Vulkan present call not found".into());
    }
    log(&format!(
        "vkdraw: present at {}, acquire at {}, create at {}",
        module_of(present),
        module_of(acquire),
        module_of(create)
    ));
    let hooks = detour(present, acquire, create)?;
    hook_note(&plain_place(present));
    log("vkdraw: hooked");
    Ok(hooks)
}

/// Present, acquire and swapchain-create as the throwaway device sees them: the first function in
/// each chain.
fn probe() -> Result<(usize, usize, usize), String> {
    // SAFETY: plain Vulkan setup and teardown of our own objects.
    unsafe {
        let entry = ash::Entry::load().map_err(|e| format!("vulkan-1.dll: {e}"))?;
        let app = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
        let instance = entry
            .create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None)
            .map_err(|e| format!("vkCreateInstance: {e}"))?;
        let result = (|| {
            let gpus = instance.enumerate_physical_devices().map_err(|e| format!("no GPUs: {e}"))?;
            let pick = gpus
                .iter()
                .copied()
                .find(|&g| instance.get_physical_device_properties(g).device_type == vk::PhysicalDeviceType::DISCRETE_GPU)
                .or(gpus.first().copied())
                .ok_or("no GPU")?;
            let family = instance
                .get_physical_device_queue_family_properties(pick)
                .iter()
                .position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .ok_or("no graphics queue")? as u32;
            let prio = [1.0f32];
            let qinfo = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&prio)];
            let exts = [ash::khr::swapchain::NAME.as_ptr()];
            let dev = instance
                .create_device(pick, &vk::DeviceCreateInfo::default().queue_create_infos(&qinfo).enabled_extension_names(&exts), None)
                .map_err(|e| format!("vkCreateDevice: {e}"))?;
            let gdpa = instance.fp_v1_0().get_device_proc_addr;
            let addr = |n: &CStr| gdpa(dev.handle(), n.as_ptr()).map(|f| f as usize).unwrap_or(0);
            let r = (addr(c"vkQueuePresentKHR"), addr(c"vkAcquireNextImageKHR"), addr(c"vkCreateSwapchainKHR"));
            dev.destroy_device(None);
            Ok::<_, String>(r)
        })();
        instance.destroy_instance(None);
        result
    }
}

/// Detour the three calls where the probe found them.
fn detour(present: usize, acquire: usize, create: usize) -> Result<Hooks, String> {
    let mut hooks = Vec::new();
    // SAFETY: targets are live functions with exactly these signatures; each detour has
    // Its trampoline address stored before it is enabled.
    unsafe {
        let pairs: [(usize, *const (), &AtomicUsize); 3] = [
            (present, hk_present as *const (), &ORIG_PRESENT),
            (acquire, hk_acquire as *const (), &ORIG_ACQUIRE),
            (create, hk_create as *const (), &ORIG_CREATE),
        ];
        for (target, hook, orig) in pairs {
            if target == 0 {
                continue;
            }
            let d = RawDetour::new(target as *const (), hook).map_err(|e| format!("hook {}: {e}", module_of(target)))?;
            orig.store(d.trampoline() as *const () as usize, Ordering::SeqCst);
            hooks.push(d);
        }
        for d in &hooks {
            d.enable().map_err(|e| format!("enable hook: {e}"))?;
        }
    }
    Ok(Hooks(hooks, Vec::new()))
}

unsafe extern "system" fn hk_acquire(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    timeout: u64,
    sem: vk::Semaphore,
    fence: vk::Fence,
    index: *mut u32,
) -> vk::Result {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    DEVICE.store(device.as_raw(), Ordering::SeqCst);
    // SAFETY: the trampoline runs the original function with the game's arguments.
    let r = unsafe {
        let orig: AcquireFn = std::mem::transmute(ORIG_ACQUIRE.load(Ordering::SeqCst));
        orig(device, swapchain, timeout, sem, fence, index)
    };
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

unsafe extern "system" fn hk_create(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'static>,
    alloc: *const vk::AllocationCallbacks<'static>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    // Our boxes and menu are blits into the frame, which Vulkan only allows when the swapchain was
    // made for transfers. Late injects never saw the game's flags, so one made while we are here
    // gets the flag; if the driver refuses, the game's own request goes through unchanged.
    // SAFETY: as above; info and out are the game's valid pointers.
    let mut transfer = false;
    let r = unsafe {
        let orig: CreateSwapchainFn = std::mem::transmute(ORIG_CREATE.load(Ordering::SeqCst));
        if !info.is_null() && !(*info).image_usage.contains(vk::ImageUsageFlags::TRANSFER_DST) {
            log(&format!("vkdraw: game swapchain usage {:?} lacks transfer, adding it", (*info).image_usage));
            let mut ours = *info;
            ours.image_usage |= vk::ImageUsageFlags::TRANSFER_DST;
            match orig(device, &ours, alloc, out) {
                vk::Result::SUCCESS => {
                    transfer = true;
                    vk::Result::SUCCESS
                }
                e => {
                    log(&format!("vkdraw: swapchain with transfer refused ({e:?}), made as the game asked"));
                    orig(device, info, alloc, out)
                }
            }
        } else {
            transfer = !info.is_null();
            orig(device, info, alloc, out)
        }
    };
    SWAP_GEN.fetch_add(1, Ordering::SeqCst);
    if r == vk::Result::SUCCESS && !info.is_null() && !out.is_null() {
        // SAFETY: checked non-null; the game filled them.
        let (i, sc) = unsafe { (&*info, *out) };
        log(&format!(
            "vkdraw: swapchain {}x{} format {:?} usage {:?}",
            i.image_extent.width, i.image_extent.height, i.image_format, i.image_usage
        ));
        if let Ok(mut s) = SWAP_SIZES.lock() {
            s.retain(|e| e.0 != sc.as_raw());
            s.push((sc.as_raw(), i.image_extent.width, i.image_extent.height));
        }
        if let Ok(mut s) = SAFE_SWAPS.lock() {
            s.retain(|e| *e != sc.as_raw());
            if transfer {
                s.push(sc.as_raw());
            }
        }
    }
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

fn safe_swap(sc: u64) -> bool {
    SAFE_SWAPS.lock().is_ok_and(|s| s.contains(&sc))
}

/// A swapchain we can't draw into, presented fine: ask the game to remake it, the way a window
/// resize does. 'Suboptimal' first (the frame was shown), 'out of date' 2 s later if it didn't; at
/// most twice each, then we just don't draw.
fn nudge(sc: u64, r: vk::Result) -> vk::Result {
    if r != vk::Result::SUCCESS || !(CONFIG.lock().is_ok_and(|c| c.on) || PICS.lock().is_ok_and(|p| p.iter().any(|x| x.is_some()))) {
        return r;
    }
    let Ok(mut n) = NUDGED.lock() else { return r };
    let now = Instant::now();
    let e = match n.iter_mut().find(|e| e.0 == sc) {
        Some(e) => e,
        None => {
            n.push((sc, now, 0));
            n.last_mut().unwrap()
        }
    };
    let wait = Duration::from_secs(2);
    let out = match e.2 {
        0 | 1 if e.2 == 0 || now.duration_since(e.1) > wait => vk::Result::SUBOPTIMAL_KHR,
        2 | 3 if now.duration_since(e.1) > wait * u32::from(e.2) => vk::Result::ERROR_OUT_OF_DATE_KHR,
        _ => return r,
    };
    e.2 += 1;
    if e.2 == 1 {
        e.1 = now;
    }
    log(&format!("vkdraw: frame not made for drawing, asking the game to remake it ({out:?})"));
    out
}

unsafe extern "system" fn hk_present(queue: vk::Queue, info: *const vk::PresentInfoKHR) -> vk::Result {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    let n = PRESENTS.fetch_add(1, Ordering::Relaxed);
    // A heartbeat, so a crash log shows whether frames still came and whether we drew.
    if n.is_multiple_of(300) {
        heartbeat("vkdraw", n);
    }
    // SAFETY: the trampoline runs the original present.
    let orig: PresentFn = unsafe { std::mem::transmute(ORIG_PRESENT.load(Ordering::SeqCst)) };
    let ours = if info.is_null() || BROKEN.load(Ordering::Relaxed) {
        None
    } else {
        // SAFETY: the game's present info, valid for this call.
        let pi = unsafe { &*info };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| draw(queue, pi))) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                BROKEN.store(true, Ordering::SeqCst);
                log(&format!("vkdraw: stopped drawing: {e}"));
                None
            }
            Err(_) => {
                BROKEN.store(true, Ordering::SeqCst);
                log("vkdraw: stopped drawing after a panic");
                None
            }
        }
    };
    let r = match ours {
        Some(sem) => {
            // Present waits for our copy instead of the game's work (ours waited for that).
            // SAFETY: same struct with one wait semaphore swapped in, alive across the call.
            let mut pi = unsafe { *info };
            pi.wait_semaphore_count = 1;
            pi.p_wait_semaphores = &sem;
            unsafe { orig(queue, &pi) }
        }
        // SAFETY: untouched game call.
        None => {
            let r = unsafe { orig(queue, info) };
            // SAFETY: the game's present info, valid for this call.
            match unsafe { info.as_ref() } {
                Some(pi) if pi.swapchain_count == 1 && !pi.p_swapchains.is_null() => {
                    let sc = unsafe { (*pi.p_swapchains).as_raw() };
                    if safe_swap(sc) { r } else { nudge(sc, r) }
                }
                _ => r,
            }
        }
    };
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

/// Draw into the frame being presented. Ok(None): nothing drawn, present as is.
fn draw(queue: vk::Queue, pi: &vk::PresentInfoKHR) -> Result<Option<vk::Semaphore>, String> {
    let cfg = CONFIG.lock().map(|c| *c).unwrap_or_default();
    let pics = PICS.lock().map(|m| m.clone()).unwrap_or_default();
    let menu = pics.iter().any(|p| p.is_some()).then_some(());
    let dev = DEVICE.load(Ordering::SeqCst);
    if (!drawing(&cfg) && menu.is_none()) || dev == 0 || pi.swapchain_count == 0 || pi.p_swapchains.is_null() || pi.p_image_indices.is_null() {
        return Ok(None);
    }
    // SAFETY: counts checked above; the game's arrays.
    let (sc, idx) = unsafe { (*pi.p_swapchains, *pi.p_image_indices as usize) };
    if !safe_swap(sc.as_raw()) {
        NO_SAFE.fetch_add(1, Ordering::Relaxed);
        return Ok(None);
    }
    let Some((w, h)) = screen_size(sc) else {
        if !NO_SIZE_SAID.swap(true, Ordering::Relaxed) {
            log("vkdraw: no usable game window size yet");
        }
        return Ok(None);
    };
    LAST_W.store(w as u32, Ordering::Relaxed);
    LAST_H.store(h as u32, Ordering::Relaxed);
    // The target is drawn white: taken from the mod's own target here.
    let cfg = Config { target: crate::entities::target_body(), ..cfg };
    let rects = with_circle(if cfg.on { boxes(&cfg, w, h) } else { Default::default() }, w, h);
    if rects.iter().all(|r| r.is_empty()) && menu.is_none() {
        return Ok(None);
    }
    let mut guard = GPU.lock().map_err(|_| "gpu lock poisoned")?;
    if guard.as_ref().is_some_and(|g| g.device.handle().as_raw() != dev || g.queue != queue) {
        // The game made a new device (or presents elsewhere): start over, leak the old one.
        *guard = None;
    }
    if guard.is_none() {
        // SAFETY: the game's live device, used from its own present thread.
        *guard = Some(unsafe { Gpu::new(vk::Device::from_raw(dev), queue)? });
    }
    let g = guard.as_mut().unwrap();
    // SAFETY: all objects below belong to this device and are used on the present thread.
    let r = unsafe { g.record_and_submit(pi, sc, idx, &rects, colors(&cfg.style), &pics, (w, h)) };
    if matches!(r, Ok(Some(_))) {
        DRAWN.fetch_add(1, Ordering::Relaxed);
    }
    r
}

/// The menu's own image (copied into the frame) and the buffer its pixels come through.
struct MenuGpu {
    image: vk::Image,
    image_mem: vk::DeviceMemory,
    buf: vk::Buffer,
    buf_mem: vk::DeviceMemory,
    /// The picture the image holds now (0: none yet, layout still undefined).
    generation: u64,
}

impl Gpu {
    unsafe fn new(dev: vk::Device, queue: vk::Queue) -> Result<Self, String> {
        unsafe {
            let loader = GetModuleHandleA(c"vulkan-1.dll".as_ptr() as _);
            let gdpa = GetProcAddress(loader, c"vkGetDeviceProcAddr".as_ptr() as _).ok_or("vkGetDeviceProcAddr missing")?;
            let gdpa: vk::PFN_vkGetDeviceProcAddr = std::mem::transmute(gdpa);
            let load = |n: &CStr| gdpa(dev, n.as_ptr()).map(|f| f as *const c_void).unwrap_or(std::ptr::null());
            let device = ash::Device::load_with(load, dev);
            let get_images = load(c"vkGetSwapchainImagesKHR");
            if get_images.is_null() {
                return Err("vkGetSwapchainImagesKHR missing".into());
            }
            let get_images: vk::PFN_vkGetSwapchainImagesKHR = std::mem::transmute(get_images);
            // Our commands must go to the queue family the game presents from. Family 0 is the
            // graphics family on AMD; check it really is the present queue.
            if device.get_device_queue(0, 0) != queue {
                return Err("the game presents from a queue other than graphics 0".into());
            }
            let pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default().queue_family_index(0).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(|e| format!("command pool: {e}"))?;
            let cmds = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(SLOTS as u32),
                )
                .map_err(|e| format!("command buffers: {e}"))?;
            let mut g = Gpu {
                device,
                queue,
                pool,
                cmds,
                fences: Vec::new(),
                sems: Vec::new(),
                colors: Vec::new(),
                colors_now: None,
                menu: [None, None, None, None],
                get_images,
                swapchain: vk::SwapchainKHR::null(),
                swap_gen: 0,
                images: Vec::new(),
                frame: 0,
            };
            let made = (|| {
                for _ in 0..SLOTS {
                    g.fences.push(
                        g.device
                            .create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)
                            .map_err(|e| format!("fence: {e}"))?,
                    );
                    g.sems.push(g.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(|e| format!("semaphore: {e}"))?);
                }
                for _ in 0..COLOURS {
                    let img = g
                        .device
                        .create_image(
                            &vk::ImageCreateInfo::default()
                                .image_type(vk::ImageType::TYPE_2D)
                                .format(vk::Format::R8G8B8A8_UNORM)
                                .extent(vk::Extent3D { width: 1, height: 1, depth: 1 })
                                .mip_levels(1)
                                .array_layers(1)
                                .samples(vk::SampleCountFlags::TYPE_1)
                                .tiling(vk::ImageTiling::OPTIMAL)
                                .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST),
                            None,
                        )
                        .map_err(|e| format!("image: {e}"))?;
                    let req = g.device.get_image_memory_requirements(img);
                    let mem = g
                        .device
                        .allocate_memory(
                            &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(req.memory_type_bits.trailing_zeros()),
                            None,
                        )
                        .map_err(|e| format!("memory: {e}"))?;
                    g.device.bind_image_memory(img, mem, 0).map_err(|e| format!("bind: {e}"))?;
                    g.colors.push((img, mem));
                }
                Ok::<_, String>(())
            })();
            if let Err(e) = made {
                destroy(g);
                return Err(e);
            }
            log("vkdraw: ready to draw");
            Ok(g)
        }
    }

    /// A picture slot's image and buffer, made once.
    unsafe fn menu_gpu(&mut self, slot: usize) -> Result<(), String> {
        if self.menu[slot].is_some() {
            return Ok(());
        }
        let (w, h) = slot_size(slot);
        unsafe {
            let d = &self.device;
            let image = d
                .create_image(
                    &vk::ImageCreateInfo::default()
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(vk::Format::R8G8B8A8_UNORM)
                        .extent(vk::Extent3D { width: w, height: h, depth: 1 })
                        .mip_levels(1)
                        .array_layers(1)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST),
                    None,
                )
                .map_err(|e| format!("menu image: {e}"))?;
            let req = d.get_image_memory_requirements(image);
            let image_mem = match d.allocate_memory(
                &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(req.memory_type_bits.trailing_zeros()),
                None,
            ) {
                Ok(m) => m,
                Err(e) => {
                    d.destroy_image(image, None);
                    return Err(format!("menu image memory: {e}"));
                }
            };
            let free_image = || {
                d.destroy_image(image, None);
                d.free_memory(image_mem, None);
            };
            if let Err(e) = d.bind_image_memory(image, image_mem, 0) {
                free_image();
                return Err(format!("menu image bind: {e}"));
            }
            let buf = match d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(u64::from(w * h * 4))
                    .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST),
                None,
            ) {
                Ok(b) => b,
                Err(e) => {
                    free_image();
                    return Err(format!("menu buffer: {e}"));
                }
            };
            let req = d.get_buffer_memory_requirements(buf);
            let buf_mem = match d.allocate_memory(
                &vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(req.memory_type_bits.trailing_zeros()),
                None,
            ) {
                Ok(m) => m,
                Err(e) => {
                    d.destroy_buffer(buf, None);
                    free_image();
                    return Err(format!("menu buffer memory: {e}"));
                }
            };
            if let Err(e) = d.bind_buffer_memory(buf, buf_mem, 0) {
                d.destroy_buffer(buf, None);
                d.free_memory(buf_mem, None);
                free_image();
                return Err(format!("menu buffer bind: {e}"));
            }
            self.menu[slot] = Some(MenuGpu { image, image_mem, buf, buf_mem, generation: 0 });
            log("vkdraw: menu image ready");
            Ok(())
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn record_and_submit(
        &mut self,
        pi: &vk::PresentInfoKHR,
        sc: vk::SwapchainKHR,
        idx: usize,
        rects: &[Vec<[i32; 4]>; COLOURS],
        want: [[f32; 4]; COLOURS],
        pics: &[Option<std::sync::Arc<MenuPic>>; PIC_SLOTS],
        (sw, sh): (i32, i32),
    ) -> Result<Option<vk::Semaphore>, String> {
        let pics: Vec<(usize, &MenuPic)> = pics
            .iter()
            .enumerate()
            .filter_map(|(slot, p)| Some((slot, p.as_deref()?)))
            .filter(|&(slot, m)| m.frame.w > 0 && m.frame.h > 0 && m.frame.w <= slot_size(slot).0 && m.frame.h <= slot_size(slot).1)
            .collect();
        for &(slot, _) in &pics {
            // SAFETY: our own objects on the game's device, made on its present thread.
            if let Err(e) = unsafe { self.menu_gpu(slot) }
                && !MENU_FAILED.swap(true, Ordering::Relaxed)
            {
                log(&format!("vkdraw: no menu: {e}"));
            }
        }
        unsafe {
            let d = &self.device;
            let gen_now = SWAP_GEN.load(Ordering::SeqCst);
            if self.swapchain != sc || self.swap_gen != gen_now {
                let mut n = 0u32;
                (self.get_images)(d.handle(), sc, &mut n, std::ptr::null_mut()).result().map_err(|e| format!("swapchain images: {e}"))?;
                let mut imgs = vec![vk::Image::null(); n as usize];
                (self.get_images)(d.handle(), sc, &mut n, imgs.as_mut_ptr()).result().map_err(|e| format!("swapchain images: {e}"))?;
                imgs.truncate(n as usize);
                // The present waits on a semaphore we signal. Reusing one is only safe once that
                // wait has run, which is certain when the same image comes round again, not after a
                // fixed number of frames: one per image.
                while self.sems.len() < imgs.len() {
                    self.sems.push(d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None).map_err(|e| format!("semaphore: {e}"))?);
                }
                self.images = imgs;
                self.swapchain = sc;
                self.swap_gen = gen_now;
            }
            let Some(&image) = self.images.get(idx) else {
                NO_IMAGE.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            };
            let s = self.frame % SLOTS;
            // Never stall the game: if this slot is still busy, skip drawing one frame.
            if d.wait_for_fences(&[self.fences[s]], true, 2_000_000).is_err() {
                BUSY.fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
            self.frame += 1;
            let cmd = self.cmds[s];
            d.reset_fences(&[self.fences[s]]).map_err(|e| format!("reset fence: {e}"))?;
            d.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).map_err(|e| format!("reset cmd: {e}"))?;
            d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))
                .map_err(|e| format!("begin: {e}"))?;
            let range = vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1);
            let barrier = |img, from, to, src_access, dst_access| {
                vk::ImageMemoryBarrier::default()
                    .image(img)
                    .old_layout(from)
                    .new_layout(to)
                    .src_access_mask(src_access)
                    .dst_access_mask(dst_access)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .subresource_range(range)
            };
            // Filled once, and again when the player picks new colours. Earlier frames' copies out
            // of these images are transfers too, so waiting on TRANSFER covers them.
            if self.colors_now != Some(want) {
                let to_dst: Vec<_> = self
                    .colors
                    .iter()
                    .map(|c| barrier(c.0, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::AccessFlags::empty(), vk::AccessFlags::TRANSFER_WRITE))
                    .collect();
                d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_dst);
                for (c, rgba) in self.colors.iter().zip(want) {
                    d.cmd_clear_color_image(cmd, c.0, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &vk::ClearColorValue { float32: rgba }, &[range]);
                }
                let to_src: Vec<_> = self
                    .colors
                    .iter()
                    .map(|c| barrier(c.0, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::TRANSFER_READ))
                    .collect();
                d.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &to_src);
            }
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(image, vk::ImageLayout::PRESENT_SRC_KHR, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::AccessFlags::MEMORY_READ, vk::AccessFlags::TRANSFER_WRITE)],
            );
            let layers = vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1);
            // Shadows first, so the colours land on top.
            for c in DRAW_ORDER {
                if rects[c].is_empty() {
                    continue;
                }
                let regions: Vec<_> = rects[c]
                    .iter()
                    .map(|r| {
                        vk::ImageBlit::default()
                            .src_subresource(layers)
                            .src_offsets([vk::Offset3D { x: 0, y: 0, z: 0 }, vk::Offset3D { x: 1, y: 1, z: 1 }])
                            .dst_subresource(layers)
                            .dst_offsets([vk::Offset3D { x: r[0], y: r[1], z: 0 }, vk::Offset3D { x: r[2], y: r[3], z: 1 }])
                    })
                    .collect();
                d.cmd_blit_image(cmd, self.colors[c].0, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &regions, vk::Filter::NEAREST);
            }
            for &(slot, pic) in &pics {
                let Some(mg) = self.menu[slot].as_mut() else { continue };
                if mg.generation != pic.generation {
                    // A new picture: its pixels go into the buffer (inline in the command buffer,
                    // 64 KB at a time), then into the menu image. Earlier frames still reading
                    // either one are transfers before these on this queue.
                    let f = &pic.frame;
                    let bytes = &f.rgba[..(f.w * f.h * 4) as usize];
                    let buf_barrier = |src, dst| {
                        vk::BufferMemoryBarrier::default()
                            .buffer(mg.buf)
                            .offset(0)
                            .size(vk::WHOLE_SIZE)
                            .src_access_mask(src)
                            .dst_access_mask(dst)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    };
                    d.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[buf_barrier(vk::AccessFlags::TRANSFER_READ, vk::AccessFlags::TRANSFER_WRITE)],
                        &[],
                    );
                    for (n, chunk) in bytes.chunks(65536).enumerate() {
                        d.cmd_update_buffer(cmd, mg.buf, (n * 65536) as u64, chunk);
                    }
                    let from = if mg.generation == 0 { vk::ImageLayout::UNDEFINED } else { vk::ImageLayout::TRANSFER_SRC_OPTIMAL };
                    d.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[buf_barrier(vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::TRANSFER_READ)],
                        &[barrier(mg.image, from, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::AccessFlags::TRANSFER_READ, vk::AccessFlags::TRANSFER_WRITE)],
                    );
                    let copy = vk::BufferImageCopy::default()
                        .buffer_offset(0)
                        .buffer_row_length(f.w)
                        .buffer_image_height(f.h)
                        .image_subresource(layers)
                        .image_extent(vk::Extent3D { width: f.w, height: f.h, depth: 1 });
                    d.cmd_copy_buffer_to_image(cmd, mg.buf, mg.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[copy]);
                    d.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[barrier(mg.image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::TRANSFER_READ)],
                    );
                    mg.generation = pic.generation;
                }
                // Only the panels go into the frame, clipped to the screen.
                let regions: Vec<_> = pic
                    .frame
                    .panels
                    .iter()
                    .filter_map(|p| {
                        let (x0, y0) = ((pic.x + p[0] as i32).max(0), (pic.y + p[1] as i32).max(0));
                        let (x1, y1) = ((pic.x + p[2] as i32).min(sw), (pic.y + p[3] as i32).min(sh));
                        (x1 > x0 && y1 > y0).then(|| {
                            let (sx, sy) = (x0 - pic.x, y0 - pic.y);
                            vk::ImageBlit::default()
                                .src_subresource(layers)
                                .src_offsets([vk::Offset3D { x: sx, y: sy, z: 0 }, vk::Offset3D { x: sx + x1 - x0, y: sy + y1 - y0, z: 1 }])
                                .dst_subresource(layers)
                                .dst_offsets([vk::Offset3D { x: x0, y: y0, z: 0 }, vk::Offset3D { x: x1, y: y1, z: 1 }])
                        })
                    })
                    .collect();
                if !regions.is_empty() {
                    d.cmd_blit_image(cmd, mg.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &regions, vk::Filter::NEAREST);
                }
            }
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::AccessFlags::TRANSFER_WRITE, vk::AccessFlags::empty())],
            );
            d.end_command_buffer(cmd).map_err(|e| format!("end: {e}"))?;
            let waits: &[vk::Semaphore] = if pi.wait_semaphore_count == 0 || pi.p_wait_semaphores.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(pi.p_wait_semaphores, pi.wait_semaphore_count as usize)
            };
            let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; waits.len()];
            let cmds = [cmd];
            let signal = [self.sems[idx]];
            let submit = vk::SubmitInfo::default().wait_semaphores(waits).wait_dst_stage_mask(&stages).command_buffers(&cmds).signal_semaphores(&signal);
            d.queue_submit(self.queue, &[submit], self.fences[s]).map_err(|e| format!("submit: {e}"))?;
            self.colors_now = Some(want);
            Ok(Some(self.sems[idx]))
        }
    }
}

/// The size of the picture being presented: from the swapchain if we saw it made, else the game
/// window's inside.
fn screen_size(sc: vk::SwapchainKHR) -> Option<(i32, i32)> {
    if let Some(&(_, w, h)) = SWAP_SIZES.lock().ok()?.iter().find(|e| e.0 == sc.as_raw()) {
        return Some((w as i32, h as i32));
    }
    let mut hwnd = WINDOW.load(Ordering::Relaxed) as HWND;
    // SAFETY: plain user32 queries.
    unsafe {
        let mut r: RECT = std::mem::zeroed();
        // Injected at the title screen, the first pick could be the intro video's or another small
        // helper window that later hides: no box was ever drawn. Pick again whenever the remembered
        // one is gone, hidden or tiny.
        if hwnd.is_null() || IsWindow(hwnd) == 0 || IsWindowVisible(hwnd) == 0 || GetClientRect(hwnd, &mut r) == 0 || r.right < 64 || r.bottom < 64 {
            hwnd = find_window()?;
            WINDOW.store(hwnd as isize, Ordering::Relaxed);
            if GetClientRect(hwnd, &mut r) == 0 || r.right < 64 || r.bottom < 64 {
                return None;
            }
        }
        // A swapchain keeps the size it was made with; the window can change size before the game
        // makes a new one (alt-tab, loads, resolution). Drawing by the window's size then writes
        // outside the frame, which can hang the GPU. So the first size seen for a swapchain sticks,
        // and a bigger window never widens it.
        let (w, h) = (r.right as u32, r.bottom as u32);
        if w >= 800 && h >= 600
            && let Ok(mut s) = SWAP_SIZES.lock()
        {
            s.retain(|e| e.0 != sc.as_raw());
            s.push((sc.as_raw(), w, h));
            log(&format!("vkdraw: swapchain {w}x{h} (from the window)"));
        }
        Some((r.right, r.bottom))
    }
}
