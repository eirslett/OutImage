//! `no_std` ENVIRONMENT math for programs that never touch text / random.
//!
//! Same `simrt_*` names as [`super::abi`]. Float ops go through `libm`
//! (MUSL-derived, same family as rustc's wasm `std` libm) so we do not pull
//! `std`'s panic/`fmt` rodata.

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    fn abort_message(ptr: *const u8, len: i32);
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

#[unsafe(export_name = "simrt_f64_pow")]
pub extern "C" fn f64_pow(base: f64, exponent: f64) -> f64 {
    if base == 0.0 && exponent <= 0.0 {
        trap("exponentiation undefined");
    }
    if base < 0.0 {
        if exponent != libm::trunc(exponent) {
            trap("exponentiation undefined");
        }
        return libm::pow(base, exponent);
    }
    if base > 0.0 && exponent != libm::trunc(exponent) {
        return libm::exp(exponent * libm::log(base));
    }
    libm::pow(base, exponent)
}

#[unsafe(export_name = "simrt_ln")]
pub extern "C" fn ln(x: f64) -> f64 {
    if !(x > 0.0) {
        trap("ln of non-positive argument");
    }
    libm::log(x)
}

#[unsafe(export_name = "simrt_exp")]
pub extern "C" fn exp(x: f64) -> f64 {
    libm::exp(x)
}

#[unsafe(export_name = "simrt_sin")]
pub extern "C" fn sin(x: f64) -> f64 {
    libm::sin(x)
}

#[unsafe(export_name = "simrt_cos")]
pub extern "C" fn cos(x: f64) -> f64 {
    libm::cos(x)
}

#[unsafe(export_name = "simrt_arctan")]
pub extern "C" fn arctan(x: f64) -> f64 {
    libm::atan(x)
}

#[unsafe(export_name = "simrt_addepsilon")]
pub extern "C" fn addepsilon(x: f64) -> f64 {
    x.next_up()
}

#[unsafe(export_name = "simrt_subepsilon")]
pub extern "C" fn subepsilon(x: f64) -> f64 {
    x.next_down()
}
