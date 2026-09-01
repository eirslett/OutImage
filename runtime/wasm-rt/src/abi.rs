//! Linear-memory ABI matching `env.*` imports in `src/codegen/wasm/module.rs`.

use std::ptr;

use crate::arena::with_arena;
use crate::runtime::environment;
use crate::runtime::text::TextFrame;

const FRAME_OFF_PTR: i32 = 0;
const FRAME_OFF_LEN: i32 = 4;
const FRAME_OFF_POS: i32 = 8;
const FRAME_OFF_PAD: i32 = 12;
// Host `SimrtTextFrame` also stores start/main after pad; the wasm ABI currently
// only reads ptr/len/pos/pad.
#[allow(dead_code)]
const FRAME_OFF_START: i32 = 16;
#[allow(dead_code)]
const FRAME_OFF_MAIN_LEN: i32 = 20;
const NUMERIC_FIELD_MAX: usize = 256;
const FORMAT_SCRATCH_LEN: usize = 512;

static mut FORMAT_SCRATCH: [u8; FORMAT_SCRATCH_LEN] = [0; FORMAT_SCRATCH_LEN];

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    fn abort_message(ptr: *const u8, len: i32);
    fn sysout_write(ptr: i32, len: i32);
}

fn trap(message: &str) -> ! {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        abort_message(message.as_ptr(), message.len() as i32);
        core::arch::wasm32::unreachable();
    }
    #[cfg(not(target_arch = "wasm32"))]
    panic!("{message}");
}

fn expect<T>(result: Result<T, String>) -> T {
    result.unwrap_or_else(|message| trap(&message))
}

unsafe fn read_i32(addr: i32) -> i32 {
    unsafe { ptr::read_unaligned(addr as *const i32) }
}

unsafe fn write_i32(addr: i32, value: i32) {
    unsafe { ptr::write_unaligned(addr as *mut i32, value) }
}

unsafe fn read_i64(addr: i32) -> i64 {
    unsafe { ptr::read_unaligned(addr as *const i64) }
}

unsafe fn write_i64(addr: i32, value: i64) {
    unsafe { ptr::write_unaligned(addr as *mut i64, value) }
}

fn latin1_from_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn load_frame(frame: i32) -> TextFrame {
    if frame == 0 {
        return TextFrame::notext();
    }
    let len = unsafe { read_i32(frame + FRAME_OFF_LEN) } as i64;
    let ptr = unsafe { read_i32(frame + FRAME_OFF_PTR) } as i64;
    let pos = unsafe { read_i32(frame + FRAME_OFF_POS) } as i64;
    let pad = unsafe { read_i32(frame + FRAME_OFF_PAD) };
    if len <= 0 || ptr == 0 {
        return TextFrame::notext();
    }
    if ptr < 0 || len > i32::MAX as i64 {
        trap("text frame out of range");
    }
    // WasmGC host frames (`emit_text_prepare_host_frame_gc`) copy only the
    // view into bump scratch. `start`/`main_len` still describe the original
    // GC object and must not rebase `ptr` — that would index past the snapshot
    // (and panic) on every subframe. Native linear frames that point into a
    // live main buffer also work here when `start == 1`.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let mut text = TextFrame::from_literal(&latin1_from_bytes(bytes), pad != 0);
    text.pos = pos;
    text
}

fn store_frame(frame: i32, text: &TextFrame) {
    if frame == 0 {
        return;
    }
    let len = unsafe { read_i32(frame + FRAME_OFF_LEN) };
    let ptr = unsafe { read_i32(frame + FRAME_OFF_PTR) };
    if len > 0 && ptr != 0 && !text.is_notext() {
        let content = text.content();
        let mut chars = content.chars();
        for i in 0..len {
            let byte = chars
                .next()
                .map(|ch| (ch as u32).min(255) as u8)
                .unwrap_or(b' ');
            unsafe {
                (ptr as *mut u8).add(i as usize).write(byte);
            }
        }
    }
    unsafe { write_i32(frame + FRAME_OFF_POS, text.pos as i32) };
}

fn with_frame<T>(frame: i32, f: impl FnOnce(&mut TextFrame) -> T) -> T {
    with_arena(|| {
        let mut text = load_frame(frame);
        let result = f(&mut text);
        store_frame(frame, &text);
        result
    })
}

fn pad_numeric_field(item: &str, w: i64) -> String {
    let mut item: String = item.chars().take(NUMERIC_FIELD_MAX).collect();
    if w == 0 {
        return item;
    }
    let width = w.unsigned_abs() as usize;
    let width = width.min(NUMERIC_FIELD_MAX);
    if item.len() > width {
        return "*".repeat(width);
    }
    let pad = width - item.len();
    if w > 0 {
        format!("{:width$}{item}", "", width = pad)
    } else {
        item.push_str(&" ".repeat(pad));
        item
    }
}

fn putreal_item(value: f64, n: i64, exp_digits: i64) -> String {
    with_arena(|| {
        let mut tmp = expect(TextFrame::blanks(NUMERIC_FIELD_MAX as i64));
        let result = if exp_digits >= 3 {
            tmp.edit_putreal_long_with(value, n, '.', '&')
        } else {
            tmp.edit_putreal_with(value, n, '.', '&')
        };
        expect(result);
        tmp.content().trim().to_string()
    })
}

fn putfix_item(value: f64, places: i64) -> String {
    with_arena(|| {
        let mut tmp = expect(TextFrame::blanks(NUMERIC_FIELD_MAX as i64));
        expect(tmp.edit_putfix_with(value, places, '.'));
        tmp.content().trim().to_string()
    })
}

fn putfrac_item(value: i64, places: i64) -> String {
    with_arena(|| {
        let mut tmp = expect(TextFrame::blanks(NUMERIC_FIELD_MAX as i64));
        expect(tmp.edit_putfrac_with(value, places, '.'));
        tmp.content().trim().to_string()
    })
}

fn write_formatted(dst: i32, cap: i32, text: &str) -> i32 {
    let cap = cap.max(0) as usize;
    let bytes: Vec<u8> = text.chars().map(|ch| (ch as u32).min(255) as u8).collect();
    let n = bytes.len().min(cap);
    if dst != 0 && n > 0 {
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, n);
        }
    }
    n as i32
}

fn stream_ptr(ptr: i64) -> i32 {
    i32::try_from(ptr).unwrap_or_else(|_| trap("random stream pointer out of range"))
}

fn with_stream<T>(ptr: i64, f: impl FnOnce(&mut i64) -> Result<T, String>) -> T {
    with_arena(|| {
        let addr = stream_ptr(ptr);
        let mut stream = unsafe { read_i64(addr) };
        let result = expect(f(&mut stream));
        unsafe { write_i64(addr, stream) };
        result
    })
}

// Export as `simrt_*` so names never collide with libm (`exp`, `sin`,
// `cos`, `log`, …). `#[no_mangle] extern "C" fn exp` would replace libm and
// recurse until the stack blew (Box–Muller `normal` calls `ln`/`cos`).
#[unsafe(export_name = "simrt_format_scratch")]
pub extern "C" fn format_scratch() -> i32 {
    &raw mut FORMAT_SCRATCH as i32
}

#[unsafe(export_name = "simrt_format_scratch_cap")]
pub extern "C" fn format_scratch_cap() -> i32 {
    FORMAT_SCRATCH_LEN as i32
}

#[unsafe(export_name = "simrt_format_out_real")]
pub extern "C" fn format_out_real(
    dst: i32,
    cap: i32,
    value: f64,
    n: i64,
    w: i64,
    exp_digits: i64,
) -> i32 {
    with_arena(|| {
        let field = if w == 0 {
            let item = putreal_item(value, n, exp_digits);
            let width = item.len().max(1) as i64;
            pad_numeric_field(&item, width)
        } else {
            pad_numeric_field(&putreal_item(value, n, exp_digits), w)
        };
        write_formatted(dst, cap, &field)
    })
}

#[unsafe(export_name = "simrt_format_out_fix")]
pub extern "C" fn format_out_fix(dst: i32, cap: i32, value: f64, n: i64, w: i64) -> i32 {
    with_arena(|| {
        let item = putfix_item(value, n);
        let field = if w == 0 {
            pad_numeric_field(&item, item.len().max(1) as i64)
        } else {
            pad_numeric_field(&item, w)
        };
        write_formatted(dst, cap, &field)
    })
}

#[unsafe(export_name = "simrt_format_out_frac")]
pub extern "C" fn format_out_frac(dst: i32, cap: i32, value: i64, n: i64, w: i64) -> i32 {
    with_arena(|| {
        let item = putfrac_item(value, n);
        let field = if w == 0 {
            pad_numeric_field(&item, item.len().max(1) as i64)
        } else {
            pad_numeric_field(&item, w)
        };
        write_formatted(dst, cap, &field)
    })
}

#[unsafe(export_name = "simrt_text_getint")]
pub extern "C" fn text_getint(frame: i32) -> i64 {
    with_frame(frame, |text| expect(text.deedit_getint()))
}

#[unsafe(export_name = "simrt_text_putint")]
pub extern "C" fn text_putint(frame: i32, value: i64) {
    with_frame(frame, |text| expect(text.edit_putint(value)));
}

#[unsafe(export_name = "simrt_text_getfrac")]
pub extern "C" fn text_getfrac(frame: i32) -> i64 {
    with_frame(frame, |text| expect(text.deedit_getfrac()))
}

#[unsafe(export_name = "simrt_text_putfrac")]
pub extern "C" fn text_putfrac(frame: i32, value: i64, places: i64) {
    with_frame(frame, |text| expect(text.edit_putfrac(value, places)));
}

#[unsafe(export_name = "simrt_text_getreal")]
pub extern "C" fn text_getreal(frame: i32) -> f64 {
    with_frame(frame, |text| expect(text.deedit_getreal()))
}

#[unsafe(export_name = "simrt_text_putfix")]
pub extern "C" fn text_putfix(frame: i32, value: f64, places: i64) {
    with_frame(frame, |text| expect(text.edit_putfix(value, places)));
}

#[unsafe(export_name = "simrt_text_putreal")]
pub extern "C" fn text_putreal(frame: i32, value: f64, n: i64, exp_digits: i64) {
    with_frame(frame, |text| {
        let result = if exp_digits >= 3 {
            text.edit_putreal_long_with(value, n, '.', '&')
        } else {
            text.edit_putreal_with(value, n, '.', '&')
        };
        expect(result);
    });
}

#[unsafe(export_name = "simrt_out_real")]
pub extern "C" fn out_real(value: f64, n: i64, w: i64, exp_digits: i64) {
    with_arena(|| {
        let len = format_out_real(
            format_scratch(),
            FORMAT_SCRATCH_LEN as i32,
            value,
            n,
            w,
            exp_digits,
        );
        emit_sysout_n(len);
    });
}

fn emit_sysout_n(len: i32) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        sysout_write(format_scratch(), len);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = len;
}

#[unsafe(export_name = "simrt_out_fix")]
pub extern "C" fn out_fix(value: f64, n: i64, w: i64) {
    with_arena(|| {
        let len = format_out_fix(format_scratch(), FORMAT_SCRATCH_LEN as i32, value, n, w);
        emit_sysout_n(len);
    });
}

#[unsafe(export_name = "simrt_out_frac")]
pub extern "C" fn out_frac(value: i64, n: i64, w: i64) {
    with_arena(|| {
        let len = format_out_frac(format_scratch(), FORMAT_SCRATCH_LEN as i32, value, n, w);
        emit_sysout_n(len);
    });
}

#[unsafe(export_name = "simrt_f64_pow")]
pub extern "C" fn f64_pow(base: f64, exponent: f64) -> f64 {
    if base == 0.0 && exponent <= 0.0 {
        trap("exponentiation undefined");
    }
    if base < 0.0 {
        if exponent != exponent.trunc() {
            trap("exponentiation undefined");
        }
        return base.powf(exponent);
    }
    if base > 0.0 && exponent != exponent.trunc() {
        return (exponent * base.ln()).exp();
    }
    base.powf(exponent)
}

#[unsafe(export_name = "simrt_ln")]
pub extern "C" fn ln(x: f64) -> f64 {
    if x.is_nan() || x <= 0.0 {
        trap("ln of non-positive argument");
    }
    x.ln()
}

#[unsafe(export_name = "simrt_exp")]
pub extern "C" fn exp(x: f64) -> f64 {
    x.exp()
}

#[unsafe(export_name = "simrt_sin")]
pub extern "C" fn sin(x: f64) -> f64 {
    x.sin()
}

#[unsafe(export_name = "simrt_cos")]
pub extern "C" fn cos(x: f64) -> f64 {
    x.cos()
}

#[unsafe(export_name = "simrt_arctan")]
pub extern "C" fn arctan(x: f64) -> f64 {
    x.atan()
}

#[unsafe(export_name = "simrt_addepsilon")]
pub extern "C" fn addepsilon(x: f64) -> f64 {
    environment::addepsilon(x)
}

#[unsafe(export_name = "simrt_subepsilon")]
pub extern "C" fn subepsilon(x: f64) -> f64 {
    environment::subepsilon(x)
}

#[unsafe(export_name = "simrt_randint")]
pub extern "C" fn randint(a: i64, b: i64, stream: i64) -> i64 {
    with_stream(stream, |s| environment::randint(a, b, s))
}

#[unsafe(export_name = "simrt_uniform")]
pub extern "C" fn uniform(a: f64, b: f64, stream: i64) -> f64 {
    with_stream(stream, |s| environment::uniform(a, b, s))
}

#[unsafe(export_name = "simrt_normal")]
pub extern "C" fn normal(a: f64, b: f64, stream: i64) -> f64 {
    with_stream(stream, |s| environment::normal(a, b, s))
}

#[unsafe(export_name = "simrt_negexp")]
pub extern "C" fn negexp(a: f64, stream: i64) -> f64 {
    with_stream(stream, |s| environment::negexp(a, s))
}

#[unsafe(export_name = "simrt_draw")]
pub extern "C" fn draw(a: f64, stream: i64) -> i64 {
    with_stream(stream, |s| {
        environment::draw(a, s).map(i64::from)
    })
}
