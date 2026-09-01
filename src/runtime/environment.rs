//! Simula ENVIRONMENT procedures (Standard Chapter 9).
//!
//! Pure Rust implementations used by the interpreter and unit-tested in isolation.

// `Instant` exists on WASI; it does not on `wasm32-unknown-unknown` (browser).
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::runtime::text::TextFrame;

/// Number of bits `n` in the pseudo-random stream algorithm (§9.9.1).
pub const STREAM_BITS: u32 = 31;
/// Parameter `p` in the pseudo-random stream algorithm (§9.9.1).
pub const STREAM_P: u32 = 13;
/// Modulus `2^n` for the pseudo-random stream.
pub const STREAM_MODULUS: i64 = 1 << STREAM_BITS;
/// Multiplier `5^(2p+1)` for the pseudo-random stream.
pub const STREAM_MULTIPLIER: i128 = 5i128.pow(2 * STREAM_P + 1);

/// Poisson parameter threshold above which normal approximation is used (§9.9).
pub const POISSON_NORMAL_THRESHOLD: f64 = 20.0;

/// Largest legal argument to `char` (§9.6).
pub const MAXRANK: i64 = 255;
/// Largest Simula integer value exposed by ENVIRONMENT.
/// DosTestBatch (and classic 32-bit implementations) expect 32-bit extrema.
pub const MAXINT: i64 = i32::MAX as i64;
/// Smallest Simula integer value exposed by ENVIRONMENT.
pub const MININT: i64 = i32::MIN as i64;
/// Largest `real` magnitude.
/// DosTestBatch expects IEEE single-precision extrema (`simtst04`/`simtst05`).
pub const MAXREAL: f64 = f32::MAX as f64;
/// Smallest positive normalized `real`.
pub const MINREAL: f64 = f32::MIN_POSITIVE as f64;
/// Largest `long real` magnitude.
pub const MAXLONGREAL: f64 = f64::MAX;
/// Smallest positive normalized `long real`.
pub const MINLONGREAL: f64 = f64::MIN_POSITIVE;

/// Mutable ENVIRONMENT state (`CURRENTLOWTEN`, `CURRENTDECIMALMARK`, CPU timer).
#[derive(Debug, Clone)]
pub struct EnvironmentRuntimeState {
    pub current_lowten: char,
    pub current_decimal_mark: char,
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    cpu_origin: Instant,
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    _cpu_origin: (),
}

impl Default for EnvironmentRuntimeState {
    fn default() -> Self {
        Self {
            current_lowten: '&',
            current_decimal_mark: '.',
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            cpu_origin: Instant::now(),
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            _cpu_origin: (),
        }
    }
}

impl EnvironmentRuntimeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset_cpu_timer(&mut self) {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            self.cpu_origin = Instant::now();
        }
    }
}

/// Implementation-defined `simulaid` string (§9.6).
pub fn simulaid_string() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into());
    format!(
        "sim/{}!!!local!!!{}!!!{}!!!{}!!!{}!!!local!!!sim",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        user,
        process_id(),
    )
}

fn process_id() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        1
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::process::id()
    }
}

/// Integer ENVIRONMENT constant value when `name` matches (§9.6).
pub fn environment_constant_i64(name: &str) -> Option<i64> {
    match name.to_ascii_lowercase().as_str() {
        "maxrank" => Some(MAXRANK),
        "maxint" => Some(MAXINT),
        "minint" => Some(MININT),
        _ => None,
    }
}

/// Real ENVIRONMENT constant value when `name` matches (§9.6).
pub fn environment_constant_f64(name: &str) -> Option<f64> {
    match name.to_ascii_lowercase().as_str() {
        "maxreal" => Some(MAXREAL),
        "minreal" => Some(MINREAL),
        "maxlongreal" => Some(MAXLONGREAL),
        "minlongreal" => Some(MINLONGREAL),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// §9.1 Basic operations
// ---------------------------------------------------------------------------

/// Integer division truncating toward zero (Simula `//`).
pub fn int_div(i: i64, j: i64) -> Result<i64, String> {
    if j == 0 {
        return Err("integer division by zero".into());
    }
    Ok(i / j)
}

/// Remainder of integer division (§9.1 `rem`).
pub fn rem(i: i64, j: i64) -> Result<i64, String> {
    Ok(i - int_div(i, j)? * j)
}

/// Mathematical modulo (§9.1 `mod`).
pub fn mod_(i: i64, j: i64) -> Result<i64, String> {
    let res = rem(i, j)?;
    if res == 0 {
        Ok(0)
    } else if sign(res) != sign(j) {
        Ok(res + j)
    } else {
        Ok(res)
    }
}

pub fn abs_integer(i: i64) -> i64 {
    i.abs()
}

pub fn abs_real(r: f64) -> f64 {
    r.abs()
}

pub fn sign(i: i64) -> i64 {
    if i > 0 {
        1
    } else if i < 0 {
        -1
    } else {
        0
    }
}

/// Sign of a real value (§9.1): −1, 0, or +1 without truncating the magnitude.
pub fn sign_real(r: f64) -> i64 {
    if r > 0.0 {
        1
    } else if r < 0.0 {
        -1
    } else {
        0
    }
}

/// Integer floor of a real value (§9.1 `entier`).
pub fn entier(r: f64) -> i64 {
    let j = r as i64;
    if (j as f64) > r { j - 1 } else { j }
}

pub fn addepsilon(r: f64) -> f64 {
    // Prefer nextafter so subepsilon(0) is -min-denormal, not a bit-wrap NaN.
    r.next_up()
}

pub fn subepsilon(r: f64) -> f64 {
    r.next_down()
}

// ---------------------------------------------------------------------------
// §9.2 Text utilities
// ---------------------------------------------------------------------------

pub fn char_code(i: i64) -> Result<char, String> {
    if !(0..=MAXRANK).contains(&i) {
        return Err(format!("char argument out of range: {i}"));
    }
    char::from_u32(i as u32).ok_or_else(|| format!("char argument out of range: {i}"))
}

pub fn isochar_code(i: i64) -> Result<char, String> {
    if !(0..=255).contains(&i) {
        return Err(format!("isochar argument out of range: {i}"));
    }
    char::from_u32(i as u32).ok_or_else(|| format!("isochar argument out of range: {i}"))
}

pub fn rank_char(c: char) -> i64 {
    c as u32 as i64
}

pub fn isorank_char(c: char) -> i64 {
    c as u32 as i64
}

pub fn digit_char(c: char) -> bool {
    c.is_ascii_digit()
}

pub fn letter_char(c: char) -> bool {
    c.is_ascii_alphabetic()
}

pub fn is_valid_lowten(c: char) -> bool {
    if digit_char(c) {
        return false;
    }
    match c {
        '+' | '-' | '.' | ',' => false,
        _ => {
            let code = c as u32;
            !(code < 32 || code == 127 || code > 127)
        }
    }
}

pub fn lowten(state: &mut EnvironmentRuntimeState, c: char) -> Result<char, String> {
    if !is_valid_lowten(c) {
        return Err("illegal lowten character".into());
    }
    let previous = state.current_lowten;
    state.current_lowten = c;
    Ok(previous)
}

pub fn decimalmark(state: &mut EnvironmentRuntimeState, c: char) -> Result<char, String> {
    if c != '.' && c != ',' {
        return Err("decimalmark must be '.' or ','".into());
    }
    let previous = state.current_decimal_mark;
    state.current_decimal_mark = c;
    Ok(previous)
}

// ---------------------------------------------------------------------------
// §9.4 Mathematical functions
// ---------------------------------------------------------------------------

pub fn sqrt_real(r: f64) -> Result<f64, String> {
    if r < 0.0 {
        return Err("sqrt of negative number".into());
    }
    Ok(r.sqrt())
}

pub fn sin_real(r: f64) -> f64 {
    r.sin()
}

pub fn cos_real(r: f64) -> f64 {
    r.cos()
}

pub fn tan_real(r: f64) -> f64 {
    r.tan()
}

pub fn cotan_real(r: f64) -> f64 {
    1.0 / r.tan()
}

pub fn arcsin_real(r: f64) -> Result<f64, String> {
    if !(-1.0..=1.0).contains(&r) {
        return Err("arcsin domain error".into());
    }
    Ok(r.asin())
}

pub fn arccos_real(r: f64) -> Result<f64, String> {
    if !(-1.0..=1.0).contains(&r) {
        return Err("arccos domain error".into());
    }
    Ok(r.acos())
}

pub fn arctan_real(r: f64) -> f64 {
    r.atan()
}

pub fn arctan2_real(y: f64, x: f64) -> Result<f64, String> {
    if y == 0.0 && x == 0.0 {
        return Err("arctan2(0,0) undefined".into());
    }
    Ok(y.atan2(x))
}

pub fn sinh_real(r: f64) -> f64 {
    r.sinh()
}

pub fn cosh_real(r: f64) -> f64 {
    r.cosh()
}

pub fn tanh_real(r: f64) -> f64 {
    r.tanh()
}

pub fn ln_real(r: f64) -> Result<f64, String> {
    if r <= 0.0 {
        return Err("ln domain error".into());
    }
    Ok(r.ln())
}

pub fn log10_real(r: f64) -> Result<f64, String> {
    if r <= 0.0 {
        return Err("log10 domain error".into());
    }
    Ok(r.log10())
}

pub fn exp_real(r: f64) -> f64 {
    r.exp()
}

// ---------------------------------------------------------------------------
// §9.5 Extremum functions
// ---------------------------------------------------------------------------

pub fn max_integer(a: i64, b: i64) -> i64 {
    a.max(b)
}

pub fn min_integer(a: i64, b: i64) -> i64 {
    a.min(b)
}

pub fn max_real(a: f64, b: f64) -> f64 {
    a.max(b)
}

pub fn min_real(a: f64, b: f64) -> f64 {
    a.min(b)
}

pub fn max_char(a: char, b: char) -> char {
    if a >= b { a } else { b }
}

pub fn min_char(a: char, b: char) -> char {
    if a <= b { a } else { b }
}

pub fn max_text(a: &str, b: &str) -> String {
    if a >= b { a.to_string() } else { b.to_string() }
}

pub fn min_text(a: &str, b: &str) -> String {
    if a <= b { a.to_string() } else { b.to_string() }
}

// ---------------------------------------------------------------------------
// §9.9 Random drawing
// ---------------------------------------------------------------------------

/// Advance a pseudo-random stream seed (§9.9.1).
pub fn advance_stream(u: i64) -> i64 {
    let product = (u as i128).wrapping_mul(STREAM_MULTIPLIER);
    rem_i128(product, STREAM_MODULUS as i128)
}

fn rem_i128(i: i128, j: i128) -> i64 {
    let q = i / j;
    (i - q * j) as i64
}

/// Convert a stream seed to a basic drawing in `[0, 1)`.
pub fn basic_draw_value(u: i64) -> f64 {
    (u as f64) * 2f64.powi(-(STREAM_BITS as i32))
}

/// Perform one basic drawing, updating `stream` in place.
///
/// A negative non-zero stream seed yields antithetic drawings `1-u` while
/// keeping the stored seed negative (§9.9.1 / S-PORT `basic_draw`).
pub fn basic_draw(stream: &mut i64) -> Result<f64, String> {
    let antithetic = *stream < 0;
    let seed = if antithetic { -*stream } else { *stream };
    let next = advance_stream(seed);
    *stream = if antithetic { -next } else { next };
    let val = basic_draw_value(next);
    if antithetic {
        Ok(if val != 0.0 { 1.0 - val } else { 0.0 })
    } else {
        Ok(val)
    }
}

pub fn draw(a: f64, stream: &mut i64) -> Result<bool, String> {
    if a >= 1.0 {
        return Ok(true);
    }
    if a <= 0.0 {
        return Ok(false);
    }
    Ok(basic_draw(stream)? < a)
}

pub fn randint(a: i64, b: i64, stream: &mut i64) -> Result<i64, String> {
    if b < a {
        return Err("randint: b < a".into());
    }
    let span = b - a + 1;
    let u = basic_draw(stream)?;
    Ok(a + entier(u * span as f64))
}

pub fn uniform(a: f64, b: f64, stream: &mut i64) -> Result<f64, String> {
    if b < a {
        return Err("uniform: b < a".into());
    }
    let u = basic_draw(stream)?;
    Ok(a + u * (b - a))
}

/// Normal distribution with mean `a` and standard deviation `b` (Box-Muller, one stream step).
pub fn normal(a: f64, b: f64, stream: &mut i64) -> Result<f64, String> {
    let u = basic_draw(stream)?;
    if u == 0.0 {
        return Ok(a);
    }
    let z = (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * u).cos();
    Ok(a + b * z)
}

pub fn negexp(a: f64, stream: &mut i64) -> Result<f64, String> {
    if a <= 0.0 {
        return Err("negexp: non-positive rate".into());
    }
    let u = basic_draw(stream)?;
    if u == 0.0 {
        return Ok(f64::INFINITY);
    }
    Ok(-u.ln() / a)
}

pub fn poisson(a: f64, stream: &mut i64) -> Result<i64, String> {
    if a <= 0.0 {
        return Ok(0);
    }
    if a > POISSON_NORMAL_THRESHOLD {
        let sample = normal(a, a.sqrt(), stream)?;
        return Ok(entier(sample + 0.5).max(0));
    }
    let threshold = (-a).exp();
    let mut product = 1.0;
    let mut n = 0;
    loop {
        product *= basic_draw(stream)?;
        if product < threshold {
            return Ok(n);
        }
        n += 1;
    }
}

pub fn erlang(a: f64, b: f64, stream: &mut i64) -> Result<f64, String> {
    if a <= 0.0 || b <= 0.0 {
        return Err("erlang: parameters must be positive".into());
    }
    let c = entier(b);
    if (c as f64) == b && c > 0 {
        let mut sum = 0.0;
        for _ in 0..c {
            let u = basic_draw(stream)?;
            if u == 0.0 {
                return Ok(f64::INFINITY);
            }
            sum += u.ln();
        }
        return Ok(-sum / (a * b));
    }
    let mut sum = 0.0;
    for _ in 0..c {
        let u = basic_draw(stream)?;
        if u == 0.0 {
            return Ok(f64::INFINITY);
        }
        sum += u.ln();
    }
    let u = basic_draw(stream)?;
    if u == 0.0 {
        return Ok(f64::INFINITY);
    }
    sum += (b - c as f64) * u.ln();
    Ok(-sum / (a * b))
}

/// Discrete distribution from a cumulative step function `a`, augmented with `1` on the right.
///
/// Returns a **1-based** Simula subscript index.
pub fn discrete(a: &[f64], stream: &mut i64) -> Result<i64, String> {
    if a.is_empty() {
        return Err("discrete: empty distribution".into());
    }
    let u = basic_draw(stream)?;
    for (index, &value) in a.iter().enumerate() {
        if value > u {
            return Ok(index as i64 + 1);
        }
    }
    Ok(a.len() as i64 + 1)
}

/// Linear interpolation distribution (§9.9 `linear`).
///
/// `a` and `b` must have equal length; `a[0]=0`, `a[n-1]=1`, monotonically non-decreasing.
pub fn linear(a: &[f64], b: &[f64], stream: &mut i64) -> Result<f64, String> {
    if a.len() != b.len() || a.is_empty() {
        return Err("linear: invalid table".into());
    }
    let u = basic_draw(stream)?;
    for i in 1..a.len() {
        if u <= a[i] {
            let d = a[i] - a[i - 1];
            if d == 0.0 {
                return Ok(b[i - 1]);
            }
            return Ok(b[i - 1] + (b[i] - b[i - 1]) * (u - a[i - 1]) / d);
        }
    }
    Ok(*b.last().unwrap_or(&0.0))
}

/// Draw from a histogram of relative frequencies (§9.9 `histd`).
///
/// Returns a **1-based** index into `a`.
pub fn histd(a: &[f64], stream: &mut i64) -> Result<i64, String> {
    if a.is_empty() {
        return Err("histd: empty histogram".into());
    }
    let total: f64 = a.iter().sum();
    if total <= 0.0 {
        return Err("histd: non-positive total frequency".into());
    }
    let target = basic_draw(stream)? * total;
    let mut cumulative = 0.0;
    for (index, &weight) in a.iter().enumerate() {
        cumulative += weight;
        if target < cumulative {
            return Ok(index as i64 + 1);
        }
    }
    Ok(a.len() as i64)
}

// ---------------------------------------------------------------------------
// §9.10 Calendar and timing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

pub fn format_datetime(dt: &LocalDateTime) -> String {
    format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millis:03}",
        year = dt.year,
        month = dt.month,
        day = dt.day,
        hour = dt.hour,
        minute = dt.minute,
        second = dt.second,
        millis = dt.millis,
    )
}

pub fn local_datetime_now() -> LocalDateTime {
    local_datetime_from_system(SystemTime::now())
}

fn local_datetime_from_system(now: SystemTime) -> LocalDateTime {
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_secs = duration.as_secs();
    let millis = duration.subsec_millis();

    let (year, month, day, hour, minute, second) = unix_seconds_to_local(total_secs as i64);
    LocalDateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
        millis,
    }
}

fn unix_seconds_to_local(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    #[cfg(unix)]
    {
        unix_localtime(secs)
    }
    #[cfg(not(unix))]
    {
        unix_utc_fallback(secs)
    }
}

#[cfg(unix)]
fn unix_localtime(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: i64,
        tm_zone: *const u8,
    }

    unsafe extern "C" {
        fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
    }

    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    unsafe {
        localtime_r(&secs, &mut tm);
    }
    (
        tm.tm_year + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    )
}

#[cfg(not(unix))]
fn unix_utc_fallback(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let day_seconds = secs.rem_euclid(86_400);
    let hour = (day_seconds / 3600) as u32;
    let minute = ((day_seconds % 3600) / 60) as u32;
    let second = (day_seconds % 60) as u32;

    let mut y = 1970i32;
    let mut remaining_days = days;
    loop {
        let days_in_year = if is_leap_year(y) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }

    let month_lengths = if is_leap_year(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 1u32;
    let mut d = remaining_days as u32 + 1;
    for &len in &month_lengths {
        if d <= len {
            break;
        }
        d -= len;
        m += 1;
    }
    (y, m, d, hour, minute, second)
}

#[cfg(not(unix))]
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

pub fn datetime_text() -> TextFrame {
    TextFrame::from_literal(&format_datetime(&local_datetime_now()), true)
}

pub fn cputime(state: &EnvironmentRuntimeState) -> f64 {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        state.cpu_origin.elapsed().as_secs_f64()
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        let _ = state;
        0.0
    }
}

pub fn clocktime() -> f64 {
    let dt = local_datetime_now();
    dt.hour as f64 * 3600.0 + dt.minute as f64 * 60.0 + dt.second as f64 + dt.millis as f64 / 1000.0
}

// ---------------------------------------------------------------------------
// §9.11 Miscellaneous
// ---------------------------------------------------------------------------

/// Update histogram `a` with observation `c` and weight `d` (§9.11 `histo`).
///
/// `a` must have length `b.len() + 1`. Returns the 0-based index updated.
pub fn histo(a: &mut [f64], b: &[f64], c: f64, d: f64) -> Result<usize, String> {
    if a.len() != b.len() + 1 {
        return Err("histo: A length must be one greater than B".into());
    }
    let index = b.iter().position(|&bound| c <= bound).unwrap_or(b.len());
    a[index] += d;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rem_matches_truncating_integer_division() {
        assert_eq!(rem(7, 3).unwrap(), 1);
        assert_eq!(rem(-7, 3).unwrap(), -1);
        assert_eq!(rem(7, -3).unwrap(), 1);
        assert_eq!(rem(-7, -3).unwrap(), -1);
    }

    #[test]
    fn mod_is_mathematical_modulo() {
        assert_eq!(mod_(7, 3).unwrap(), 1);
        assert_eq!(mod_(-7, 3).unwrap(), 2);
        assert_eq!(mod_(7, -3).unwrap(), -2);
        assert_eq!(mod_(-7, -3).unwrap(), -1);
        assert_eq!(mod_(6, 3).unwrap(), 0);
    }

    #[test]
    fn int_div_truncates_toward_zero() {
        assert_eq!(int_div(7, 3).unwrap(), 2);
        assert_eq!(int_div(-7, 3).unwrap(), -2);
        assert_eq!(int_div(7, -3).unwrap(), -2);
        assert_eq!(int_div(-7, -3).unwrap(), 2);
    }

    #[test]
    fn sign_and_abs_work() {
        assert_eq!(sign(-4), -1);
        assert_eq!(sign(0), 0);
        assert_eq!(sign(9), 1);
        assert_eq!(abs_integer(-4), 4);
        assert_eq!(abs_real(-2.5), 2.5);
    }

    #[test]
    fn entier_is_floor() {
        assert_eq!(entier(1.8), 1);
        assert_eq!(entier(-1.8), -2);
        assert_eq!(entier(3.0), 3);
    }

    #[test]
    fn epsilon_steppers_change_value() {
        let x = 1.0;
        assert!(subepsilon(x) < x);
        assert!(addepsilon(x) > x);
    }

    #[test]
    fn char_and_rank_are_inverses_for_ascii() {
        assert_eq!(char_code(65).unwrap(), 'A');
        assert_eq!(rank_char('A'), 65);
        assert_eq!(isochar_code(48).unwrap(), '0');
        assert!(digit_char('5'));
        assert!(!digit_char('x'));
        assert!(letter_char('z'));
        assert!(!letter_char('5'));
    }

    #[test]
    fn char_rejects_out_of_range() {
        assert!(char_code(-1).is_err());
        assert!(char_code(MAXRANK + 1).is_err());
        assert!(isochar_code(256).is_err());
    }

    #[test]
    fn lowten_and_decimalmark_update_state() {
        let mut state = EnvironmentRuntimeState::default();
        assert_eq!(state.current_lowten, '&');
        assert_eq!(state.current_decimal_mark, '.');

        assert_eq!(lowten(&mut state, '*').unwrap(), '&');
        assert_eq!(state.current_lowten, '*');
        assert!(lowten(&mut state, '5').is_err());

        assert_eq!(decimalmark(&mut state, ',').unwrap(), '.');
        assert_eq!(state.current_decimal_mark, ',');
        assert!(decimalmark(&mut state, ';').is_err());
    }

    #[test]
    fn lowten_rejects_illegal_characters() {
        for ch in ['+', '-', '.', ',', '\x01', '\x7f'] {
            assert!(
                !is_valid_lowten(ch),
                "expected {ch:?} to be illegal as lowten"
            );
        }
    }

    #[test]
    fn math_functions_follow_spec_edge_cases() {
        assert!((sin_real(std::f64::consts::FRAC_PI_2) - 1.0).abs() < 1e-10);
        assert!((sqrt_real(4.0).unwrap() - 2.0).abs() < 1e-10);
        assert!(arcsin_real(1.1).is_err());
        assert!(ln_real(0.0).is_err());
        assert!(arctan2_real(0.0, 0.0).is_err());
        assert!((arctan2_real(1.0, 0.0).unwrap() - std::f64::consts::FRAC_PI_2).abs() < 1e-10);
    }

    #[test]
    fn extremum_functions_select_correct_value() {
        assert_eq!(max_integer(2, 5), 5);
        assert_eq!(min_integer(2, 5), 2);
        assert_eq!(max_real(1.5, 1.25), 1.5);
        assert_eq!(min_char('b', 'a'), 'a');
        assert_eq!(max_text("beta", "alpha"), "beta");
        assert_eq!(min_text("beta", "alpha"), "alpha");
    }

    #[test]
    fn constants_match_implementation_limits() {
        assert_eq!(MAXINT, i32::MAX as i64);
        assert_eq!(MININT, i32::MIN as i64);
        assert_eq!(MAXRANK, 255);
        assert!(simulaid_string().contains("sim/"));
        assert!(simulaid_string().contains("!!!"));
    }

    #[test]
    fn random_stream_advances_with_spec_multiplier() {
        let u0 = 1_i64;
        let u1 = advance_stream(u0);
        let expected = rem_i128(u0 as i128 * STREAM_MULTIPLIER, STREAM_MODULUS as i128);
        assert_eq!(u1, expected);
        assert!(u1 > 0);
        assert!(u1 < STREAM_MODULUS);
    }

    #[test]
    fn basic_draw_is_in_unit_interval() {
        let mut stream = 1_i64;
        for _ in 0..20 {
            let u = basic_draw(&mut stream).unwrap();
            assert!((0.0..1.0).contains(&u));
        }
    }

    #[test]
    fn antithetic_stream_returns_one_minus_u() {
        let mut pos = 17_i64;
        let mut neg = -17_i64;
        let u = basic_draw(&mut pos).unwrap();
        let anti = basic_draw(&mut neg).unwrap();
        assert!(neg < 0);
        assert_eq!(neg, -pos);
        if u == 0.0 {
            assert_eq!(anti, 0.0);
        } else {
            assert!((anti - (1.0 - u)).abs() < 1e-15);
        }
    }

    #[test]
    fn sign_real_does_not_truncate() {
        assert_eq!(sign_real(0.5), 1);
        assert_eq!(sign_real(-0.5), -1);
        assert_eq!(sign_real(0.0), 0);
        assert_eq!(sign(-3), -1);
    }

    #[test]
    fn draw_respects_probability_bounds() {
        let mut stream = 12345;
        assert!(draw(1.0, &mut stream).unwrap());
        assert!(!draw(0.0, &mut stream).unwrap());
    }

    #[test]
    fn randint_returns_values_in_range() {
        let mut stream = 999;
        for _ in 0..50 {
            let value = randint(3, 7, &mut stream).unwrap();
            assert!((3..=7).contains(&value));
        }
        assert!(randint(5, 4, &mut stream).is_err());
    }

    #[test]
    fn uniform_respects_interval() {
        let mut stream = 42;
        let value = uniform(2.0, 5.0, &mut stream).unwrap();
        assert!((2.0..5.0).contains(&value));
        assert!(uniform(5.0, 2.0, &mut stream).is_err());
    }

    #[test]
    fn normal_negexp_and_erlang_produce_finite_values() {
        let mut stream = 17;
        let n = normal(0.0, 1.0, &mut stream).unwrap();
        assert!(n.is_finite());
        let e = negexp(2.0, &mut stream).unwrap();
        assert!(e >= 0.0);
        let er = erlang(1.0, 2.0, &mut stream).unwrap();
        assert!(er >= 0.0);
    }

    #[test]
    fn poisson_small_and_large_parameter_paths() {
        let mut stream = 123;
        let small = poisson(3.0, &mut stream).unwrap();
        assert!(small >= 0);

        let mut stream2 = 456;
        let large = poisson(25.0, &mut stream2).unwrap();
        assert!(large >= 0);
    }

    #[test]
    fn discrete_linear_and_histd_use_distribution_tables() {
        let mut stream = 1;
        let a = [0.2, 0.7, 1.0];
        let idx = discrete(&a, &mut stream).unwrap();
        assert!((1..=4).contains(&idx));

        let mut stream2 = 1;
        let xs = [0.0, 0.5, 1.0];
        let ys = [0.0, 10.0, 20.0];
        let value = linear(&xs, &ys, &mut stream2).unwrap();
        assert!((0.0..=20.0).contains(&value));

        let mut stream3 = 1;
        let hist = [1.0, 1.0, 1.0];
        let bucket = histd(&hist, &mut stream3).unwrap();
        assert!((1..=3).contains(&bucket));
    }

    #[test]
    fn datetime_has_required_shape() {
        let text = format_datetime(&LocalDateTime {
            year: 2026,
            month: 7,
            day: 12,
            hour: 21,
            minute: 30,
            second: 45,
            millis: 123,
        });
        assert_eq!(text, "2026-07-12 21:30:45.123");
        let frame = datetime_text();
        assert!(frame.content().contains('-'));
        assert!(frame.content().contains(':'));
    }

    #[test]
    fn cputime_and_clocktime_are_non_negative() {
        let state = EnvironmentRuntimeState::new();
        assert!(cputime(&state) >= 0.0);
        assert!(clocktime() >= 0.0);
        assert!(clocktime() < 86_400.0);
    }

    #[test]
    fn histo_updates_correct_bucket() {
        let mut a = [0.0, 0.0, 0.0];
        let b = [1.0, 2.0];
        assert_eq!(histo(&mut a, &b, 0.5, 2.0).unwrap(), 0);
        assert_eq!(a[0], 2.0);
        assert_eq!(histo(&mut a, &b, 1.5, 1.0).unwrap(), 1);
        assert_eq!(a[1], 1.0);
        assert_eq!(histo(&mut a, &b, 99.0, 3.0).unwrap(), 2);
        assert_eq!(a[2], 3.0);
    }

    #[test]
    fn histo_requires_length_invariant() {
        let mut a = [0.0, 0.0];
        let b = [1.0, 2.0];
        assert!(histo(&mut a, &b, 1.0, 1.0).is_err());
    }
}
