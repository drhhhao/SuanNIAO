//! 切换方案时桌面上弹的通知（和 axonkey 的一样）：置顶、不抢焦点、鼠标能穿过去，
//! 在鼠标所在显示器的工作区里水平居中、竖直 75% 的位置，显示「方案已切换 / A → B」，2 秒后淡出。
//! 样式跟主界面的提示条一致：近黑的圆角底、白字，标题前一个青柠色小点。
//!
//! 窗口建在托盘线程上（那里有消息循环）：别的线程调 show() 只是记下名字、给托盘窗口发个消息。
//! 用分层窗口（UpdateLayeredWindow）画：圆角按像素算抗锯齿，文字用 GDI 灰度抗锯齿画成遮罩再上色。

use std::sync::Mutex;
use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GetDC, GetMonitorInfoW, MonitorFromPoint,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_END_ELLIPSIS, DT_NOPREFIX,
    DT_SINGLELINE, FF_DONTCARE, HBITMAP, HDC, HFONT, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, KillTimer, RegisterClassW, SetTimer, SetWindowPos, ShowWindow, UpdateLayeredWindow,
    HTTRANSPARENT, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_APP, WM_NCHITTEST,
    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::tray;

/// 托盘窗口收到这个消息就把通知弹出来
pub const WM_SHOW_OVERLAY: u32 = WM_APP + 2;

const TIMER_ID: usize = 1;
const TICK_MS: u32 = 15;
const FADE_IN_MS: f64 = 140.0;
/// 从弹出到开始淡出
const SHOW_MS: f64 = 2000.0;
const FADE_OUT_MS: f64 = 240.0;

/// 等着弹的（原来的方案，新的方案）
static PENDING: Mutex<Option<(String, String)>> = Mutex::new(None);
static STATE: Mutex<Option<Overlay>> = Mutex::new(None);

struct Overlay {
    hwnd: isize,
    shown_at: Instant,
    visible: bool,
    /// 弹出时的不透明度（已经开着又来一次就不再淡入）
    start_alpha: f64,
}

/// 任何线程都可以调：记下名字，让托盘线程弹出来
pub fn show(from: &str, to: &str) {
    *PENDING.lock().unwrap() = Some((from.to_string(), to.to_string()));
    tray::post(WM_SHOW_OVERLAY);
}

/// 托盘线程收到 WM_SHOW_OVERLAY 时调
pub fn on_show() {
    let Some((from, to)) = PENDING.lock().unwrap().take() else { return };
    let mut state = STATE.lock().unwrap();
    let hwnd = match state.as_ref() {
        Some(o) => HWND(o.hwnd as *mut _),
        None => match create_window() {
            Some(hwnd) => hwnd,
            None => return,
        },
    };
    let already = state.as_ref().is_some_and(|o| o.visible);
    let start_alpha = if already { current_alpha(state.as_ref().unwrap()) } else { 0.0 };
    if !paint(hwnd, &from, &to, start_alpha) {
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
        SetTimer(Some(hwnd), TIMER_ID, TICK_MS, None);
    }
    *state = Some(Overlay { hwnd: hwnd.0 as isize, shown_at: Instant::now(), visible: true, start_alpha });
}

fn create_window() -> Option<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = w!("PhonePadProfileOverlay");
        let wc = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("蒜鸟 · 方案切换"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()
    }
}

/// 现在的不透明度（0～1），按弹出后过了多久算
fn current_alpha(o: &Overlay) -> f64 {
    let t = o.shown_at.elapsed().as_secs_f64() * 1000.0;
    if t < FADE_IN_MS {
        o.start_alpha + (1.0 - o.start_alpha) * t / FADE_IN_MS
    } else if t < SHOW_MS {
        1.0
    } else {
        (1.0 - (t - SHOW_MS) / FADE_OUT_MS).max(0.0)
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 鼠标点到它就当没有（再加上 WS_EX_TRANSPARENT，点击都落到下面的窗口）
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_TIMER if wparam.0 == TIMER_ID => {
            tick(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn tick(hwnd: HWND) {
    let mut state = STATE.lock().unwrap();
    let Some(o) = state.as_mut() else { return };
    let alpha = current_alpha(o);
    let done = o.shown_at.elapsed().as_secs_f64() * 1000.0 >= SHOW_MS + FADE_OUT_MS;
    unsafe {
        if done {
            let _ = KillTimer(Some(hwnd), TIMER_ID);
            let _ = ShowWindow(hwnd, SW_HIDE);
            o.visible = false;
        } else {
            set_alpha(hwnd, alpha);
        }
    }
}

fn blend(alpha: f64) -> BLENDFUNCTION {
    BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        AlphaFormat: AC_SRC_ALPHA as u8,
    }
}

/// 内容不变，只改整体不透明度
fn set_alpha(hwnd: HWND, alpha: f64) {
    let blend = blend(alpha);
    unsafe {
        let _ = UpdateLayeredWindow(hwnd, None, None, None, None, None, COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

/// 32 位 DIB：GDI 往上画字，我们直接改像素
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Canvas {
    fn new(w: i32, h: i32) -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // 从上往下
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                _ => {
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let old = SelectObject(dc, bitmap.into());
            Some(Self { dc, bitmap, old, bits: bits.cast(), w, h })
        }
    }

    fn pixels(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

fn font(px: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            FF_DONTCARE.0 as u32,
            w!("Microsoft YaHei UI"),
        )
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// 量字宽
fn measure(dc: HDC, font: HFONT, text: &str) -> i32 {
    let mut text = wide(text);
    let mut rect = RECT::default();
    unsafe {
        let old = SelectObject(dc, font.into());
        DrawTextW(dc, &mut text, &mut rect, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old);
    }
    rect.right - rect.left
}

/// 一段字：画成遮罩（白字黑底），再按颜色叠到 out 上（out 是预乘过的 RGBA，0～1）
struct Run<'a> {
    text: &'a str,
    font: HFONT,
    color: [f64; 3],
    rect: RECT,
}

fn draw_run(canvas: &mut Canvas, out: &mut [[f64; 4]], run: &Run) {
    canvas.pixels().fill(0);
    let mut text = wide(run.text);
    let mut rect = run.rect;
    unsafe {
        let old = SelectObject(canvas.dc, run.font.into());
        SetBkMode(canvas.dc, TRANSPARENT);
        SetTextColor(canvas.dc, COLORREF(0x00FF_FFFF));
        DrawTextW(canvas.dc, &mut text, &mut rect, DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        SelectObject(canvas.dc, old);
    }
    for (pixel, o) in canvas.pixels().iter().zip(out.iter_mut()) {
        let coverage = ((pixel >> 8) & 0xFF) as f64 / 255.0;
        if coverage > 0.0 {
            for c in 0..3 {
                o[c] = run.color[c] * coverage + o[c] * (1.0 - coverage);
            }
            o[3] = coverage + o[3] * (1.0 - coverage);
        }
    }
}

/// 圆角矩形的有向距离（里面是负的）
fn rounded_distance(x: f64, y: f64, w: f64, h: f64, r: f64) -> f64 {
    let qx = (x - w / 2.0).abs() - (w / 2.0 - r);
    let qy = (y - h / 2.0).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// 画好内容、放到鼠标所在显示器上。alpha：现在的整体不透明度
fn paint(hwnd: HWND, from: &str, to: &str, alpha: f64) -> bool {
    unsafe {
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale = dpi_x.max(96) as f64 / 96.0;
        let px = |v: f64| (v * scale).round() as i32;
        let work = info.rcWork;
        let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);

        let title_font = font(px(13.0), 400);
        let name_font = font(px(19.0), 600);
        let arrow = "  →  ";
        let measure_dc = CreateCompatibleDC(None);
        let title_text = "方案已切换";
        let title_w = measure(measure_dc, title_font, title_text);
        let (mut from_w, arrow_w, mut to_w) =
            (measure(measure_dc, name_font, from), measure(measure_dc, name_font, arrow), measure(measure_dc, name_font, to));
        let _ = DeleteDC(measure_dc);

        let pad = px(30.0);
        let max_w = (work_w as f64 * 0.9) as i32;
        let names_w = from_w + arrow_w + to_w;
        let w = (names_w.max(title_w + px(14.0)) + 2 * pad).clamp(px(300.0), max_w.max(px(300.0)));
        let h = px(92.0);
        // 名字太长：两边各给一半，放不下的用省略号
        let room = w - 2 * pad - arrow_w;
        if from_w + to_w > room {
            let half = room / 2;
            if from_w > half && to_w > half {
                from_w = half;
                to_w = room - half;
            } else if from_w > half {
                from_w = room - to_w;
            } else {
                to_w = room - from_w;
            }
        }

        let Some(mut canvas) = Canvas::new(w, h) else {
            return false;
        };
        let mut out = vec![[0.0f64; 4]; (w * h) as usize];
        // 底：近黑圆角，边上一圈淡一点的线
        let radius = px(20.0) as f64;
        let bg = [0.071, 0.071, 0.071];
        let line = [0.20, 0.20, 0.20];
        let opacity = 0.96;
        for y in 0..h {
            for x in 0..w {
                let d = rounded_distance(x as f64 + 0.5, y as f64 + 0.5, w as f64, h as f64, radius);
                let coverage = (0.5 - d).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                let edge = (1.0 - (d + 1.0).abs()).clamp(0.0, 1.0);
                let a = coverage * opacity;
                let o = &mut out[(y * w + x) as usize];
                for c in 0..3 {
                    o[c] = (bg[c] * (1.0 - edge) + line[c] * edge) * a;
                }
                o[3] = a;
            }
        }
        // 标题：青柠色小点 + 灰字，整行居中
        let dot_r = px(3.5) as f64;
        let gap = px(8.0);
        let title_left = (w - (title_w + gap + 2 * dot_r as i32)) / 2;
        let title_top = px(18.0);
        let title_h = px(20.0);
        let dot_cx = title_left as f64 + dot_r;
        let dot_cy = title_top as f64 + title_h as f64 / 2.0;
        let lime = [0.745, 0.949, 0.392];
        for y in (dot_cy - dot_r - 2.0) as i32..(dot_cy + dot_r + 2.0) as i32 {
            for x in (dot_cx - dot_r - 2.0) as i32..(dot_cx + dot_r + 2.0) as i32 {
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let d = ((x as f64 + 0.5 - dot_cx).powi(2) + (y as f64 + 0.5 - dot_cy).powi(2)).sqrt() - dot_r;
                let a = (0.5 - d).clamp(0.0, 1.0);
                let o = &mut out[(y * w + x) as usize];
                for c in 0..3 {
                    o[c] = lime[c] * a + o[c] * (1.0 - a);
                }
                o[3] = a + o[3] * (1.0 - a);
            }
        }
        let title_x = title_left + 2 * dot_r as i32 + gap;
        let names_left = (w - (from_w + arrow_w + to_w)) / 2;
        let names_top = px(44.0);
        let names_h = px(30.0);
        let rect = |left: i32, width: i32, top: i32, height: i32| RECT { left, top, right: left + width, bottom: top + height };
        let runs = [
            Run { text: title_text, font: title_font, color: [0.63, 0.63, 0.63], rect: rect(title_x, title_w + px(2.0), title_top, title_h) },
            Run { text: from, font: name_font, color: [0.72, 0.72, 0.72], rect: rect(names_left, from_w, names_top, names_h) },
            Run { text: arrow, font: name_font, color: [0.50, 0.50, 0.50], rect: rect(names_left + from_w, arrow_w, names_top, names_h) },
            Run { text: to, font: name_font, color: [1.0, 1.0, 1.0], rect: rect(names_left + from_w + arrow_w, to_w + px(2.0), names_top, names_h) },
        ];
        for run in &runs {
            draw_run(&mut canvas, &mut out, run);
        }
        let _ = DeleteObject(title_font.into());
        let _ = DeleteObject(name_font.into());
        // 写回位图：预乘过的 BGRA
        for (pixel, o) in canvas.pixels().iter_mut().zip(&out) {
            let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            *pixel = (to8(o[3]) << 24) | (to8(o[0]) << 16) | (to8(o[1]) << 8) | to8(o[2]);
        }
        let position = POINT { x: work.left + (work_w - w) / 2, y: work.top + (work_h as f64 * 0.75) as i32 - h / 2 };
        let size = SIZE { cx: w, cy: h };
        let source = POINT::default();
        let screen = GetDC(None);
        let blend = blend(alpha);
        let ok = UpdateLayeredWindow(
            hwnd,
            Some(screen),
            Some(&position),
            Some(&size),
            Some(canvas.dc),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
        .is_ok();
        ReleaseDC(None, screen);
        ok
    }
}
