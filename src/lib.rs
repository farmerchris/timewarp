use libc::{c_char, c_int, clockid_t, time_t, timespec, timeval, useconds_t};
use once_cell::sync::Lazy;
use std::cell::Cell;
use std::env;
use std::ffi::CString;

#[cfg(target_os = "linux")]
use libc::RTLD_NEXT;
#[cfg(target_os = "macos")]
use libc::RTLD_NEXT;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use libc::dlsym;

static ANCHOR_REALTIME_NS: Lazy<i128> =
    Lazy::new(|| real_now_ns(libc::CLOCK_REALTIME).unwrap_or(0));
static ANCHOR_MONOTONIC_NS: Lazy<i128> =
    Lazy::new(|| real_now_ns(libc::CLOCK_MONOTONIC).unwrap_or(0));

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct Interpose {
    replacement: *const libc::c_void,
    replacee: *const libc::c_void,
}

#[cfg(target_os = "macos")]
unsafe impl Sync for Interpose {}

#[cfg(target_os = "macos")]
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_CLOCK_GETTIME: Interpose = Interpose {
    replacement: clock_gettime as *const libc::c_void,
    replacee: libc::clock_gettime as *const libc::c_void,
};

#[cfg(target_os = "macos")]
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_GETTIMEOFDAY: Interpose = Interpose {
    replacement: gettimeofday as *const libc::c_void,
    replacee: libc::gettimeofday as *const libc::c_void,
};

#[cfg(target_os = "macos")]
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_TIME: Interpose = Interpose {
    replacement: time as *const libc::c_void,
    replacee: libc::time as *const libc::c_void,
};

static REAL_CLOCK_GETTIME: Lazy<Option<unsafe extern "C" fn(clockid_t, *mut timespec) -> c_int>> =
    Lazy::new(|| unsafe { resolve_symbol("clock_gettime") });
static REAL_GETTIMEOFDAY: Lazy<
    Option<unsafe extern "C" fn(*mut timeval, *mut libc::c_void) -> c_int>,
> = Lazy::new(|| unsafe { resolve_symbol("gettimeofday") });
static REAL_NANOSLEEP: Lazy<Option<unsafe extern "C" fn(*const timespec, *mut timespec) -> c_int>> =
    Lazy::new(|| unsafe { resolve_symbol("nanosleep") });
static REAL_USLEEP: Lazy<Option<unsafe extern "C" fn(useconds_t) -> c_int>> =
    Lazy::new(|| unsafe { resolve_symbol("usleep") });
static REAL_SLEEP: Lazy<Option<unsafe extern "C" fn(libc::c_uint) -> libc::c_uint>> =
    Lazy::new(|| unsafe { resolve_symbol("sleep") });

#[derive(Clone, Copy)]
struct WarpState {
    offset_ns: i128,
    speed: f64,
    warp_monotonic: bool,
    scale_sleep: bool,
}

unsafe fn resolve_symbol<T>(name: &str) -> Option<T> {
    let c_name = CString::new(name).ok()?;
    let ptr = unsafe { dlsym(RTLD_NEXT, c_name.as_ptr() as *const c_char) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

fn read_state() -> WarpState {
    let offset_ns = 0_i128;
    let speed = 1.0_f64;
    let warp_monotonic = env::var("TW_WARP_MONOTONIC")
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    let scale_sleep = env::var("TW_SCALE_SLEEP")
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);

    let Some(path) = env::var_os("TW_STATE_PATH") else {
        return WarpState {
            offset_ns,
            speed,
            warp_monotonic,
            scale_sleep,
        };
    };

    let Ok(raw) = std::fs::read_to_string(path) else {
        return WarpState {
            offset_ns,
            speed,
            warp_monotonic,
            scale_sleep,
        };
    };

    let mut parts = raw.split_whitespace();
    let parsed_offset = parts.next().and_then(|p| p.parse::<i128>().ok());
    let parsed_speed = parts.next().and_then(|p| p.parse::<f64>().ok());

    WarpState {
        offset_ns: parsed_offset.unwrap_or(offset_ns),
        speed: parsed_speed.filter(|v| *v >= 0.0).unwrap_or(speed),
        warp_monotonic,
        scale_sleep,
    }
}

fn timespec_to_ns(ts: &timespec) -> i128 {
    (ts.tv_sec as i128) * 1_000_000_000 + (ts.tv_nsec as i128)
}

fn ns_to_timespec(ns: i128) -> timespec {
    let sec = ns.div_euclid(1_000_000_000);
    let nsec = ns.rem_euclid(1_000_000_000);
    timespec {
        tv_sec: sec as libc::time_t,
        tv_nsec: nsec as libc::c_long,
    }
}

fn warp_ns(base_ns: i128, clock_id: clockid_t, state: WarpState) -> i128 {
    let anchor = match clock_id {
        libc::CLOCK_REALTIME => *ANCHOR_REALTIME_NS,
        libc::CLOCK_MONOTONIC => *ANCHOR_MONOTONIC_NS,
        _ => *ANCHOR_REALTIME_NS,
    };

    let elapsed = base_ns - anchor;
    let scaled_elapsed = (elapsed as f64) * state.speed;
    let scaled_elapsed_ns = scaled_elapsed as i128;
    anchor + state.offset_ns + scaled_elapsed_ns
}

fn should_warp(clock_id: clockid_t, state: WarpState) -> bool {
    if clock_id == libc::CLOCK_REALTIME {
        return true;
    }
    state.warp_monotonic && clock_id == libc::CLOCK_MONOTONIC
}

fn scale_sleep_duration_ns(req_ns: i128, state: WarpState) -> i128 {
    if !state.scale_sleep || state.speed <= 0.0 || (state.speed - 1.0).abs() < f64::EPSILON {
        return req_ns;
    }
    if req_ns <= 0 {
        return req_ns;
    }
    let scaled = (req_ns as f64 / state.speed) as i128;
    if scaled <= 0 { 1 } else { scaled }
}

fn with_hook_guard<T>(fallback: impl FnOnce() -> T, f: impl FnOnce() -> T) -> T {
    IN_HOOK.with(|flag| {
        if flag.get() {
            return fallback();
        }
        flag.set(true);
        let out = f();
        flag.set(false);
        out
    })
}

fn real_now_ns(clock_id: clockid_t) -> Option<i128> {
    let mut ts = timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { (*REAL_CLOCK_GETTIME)?(clock_id, &mut ts as *mut timespec) };
    if rc == 0 {
        Some(timespec_to_ns(&ts))
    } else {
        None
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_gettime(clock_id: clockid_t, tp: *mut timespec) -> c_int {
    let Some(real) = *REAL_CLOCK_GETTIME else {
        return -1;
    };

    if tp.is_null() {
        return -1;
    }

    with_hook_guard(
        || unsafe { real(clock_id, tp) },
        || {
            let rc = unsafe { real(clock_id, tp) };
            if rc != 0 {
                return rc;
            }

            let state = read_state();
            if should_warp(clock_id, state) {
                let base_ns = unsafe { timespec_to_ns(&*tp) };
                let warped_ns = warp_ns(base_ns, clock_id, state);
                unsafe {
                    *tp = ns_to_timespec(warped_ns);
                }
            }

            rc
        },
    )
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __clock_gettime(clock_id: clockid_t, tp: *mut timespec) -> c_int {
    unsafe { clock_gettime(clock_id, tp) }
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __clock_gettime64(clock_id: clockid_t, tp: *mut timespec) -> c_int {
    unsafe { clock_gettime(clock_id, tp) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn time(out: *mut time_t) -> time_t {
    let Some(real_clock_gettime) = *REAL_CLOCK_GETTIME else {
        return -1;
    };

    with_hook_guard(
        || {
            let mut ts = timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let rc = unsafe { real_clock_gettime(libc::CLOCK_REALTIME, &mut ts as *mut timespec) };
            if rc != 0 {
                return -1;
            }
            let secs = ts.tv_sec;
            if !out.is_null() {
                unsafe { *out = secs };
            }
            secs
        },
        || {
            let mut ts = timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let rc = unsafe { real_clock_gettime(libc::CLOCK_REALTIME, &mut ts as *mut timespec) };
            if rc != 0 {
                return -1;
            }

            let state = read_state();
            let base_ns = timespec_to_ns(&ts);
            let warped_ns = warp_ns(base_ns, libc::CLOCK_REALTIME, state);
            let warped_secs = warped_ns.div_euclid(1_000_000_000) as time_t;

            if !out.is_null() {
                unsafe {
                    *out = warped_secs;
                }
            }

            warped_secs
        },
    )
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __time(out: *mut time_t) -> time_t {
    unsafe { time(out) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gettimeofday(tv: *mut timeval, tz: *mut libc::c_void) -> c_int {
    let Some(real) = *REAL_GETTIMEOFDAY else {
        return -1;
    };

    if tv.is_null() {
        return unsafe { real(tv, tz) };
    }

    with_hook_guard(
        || unsafe { real(tv, tz) },
        || {
            let rc = unsafe { real(tv, tz) };
            if rc != 0 {
                return rc;
            }

            let state = read_state();
            let base_ns =
                unsafe { (*tv).tv_sec as i128 * 1_000_000_000 + (*tv).tv_usec as i128 * 1_000 };
            let warped_ns = warp_ns(base_ns, libc::CLOCK_REALTIME, state);
            let sec = warped_ns.div_euclid(1_000_000_000);
            let usec = warped_ns.rem_euclid(1_000_000_000) / 1_000;

            unsafe {
                (*tv).tv_sec = sec as libc::time_t;
                (*tv).tv_usec = usec as libc::suseconds_t;
            }

            rc
        },
    )
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __gettimeofday(tv: *mut timeval, tz: *mut libc::c_void) -> c_int {
    unsafe { gettimeofday(tv, tz) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nanosleep(req: *const timespec, rem: *mut timespec) -> c_int {
    let Some(real) = *REAL_NANOSLEEP else {
        return -1;
    };
    if req.is_null() {
        return -1;
    }

    with_hook_guard(
        || unsafe { real(req, rem) },
        || {
            let state = read_state();
            let req_ns = unsafe { timespec_to_ns(&*req) };
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            let scaled_req = ns_to_timespec(scaled_ns);
            let mut scaled_rem = timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let real_rem_ptr = if rem.is_null() {
                std::ptr::null_mut()
            } else {
                &mut scaled_rem as *mut timespec
            };
            let rc = unsafe { real(&scaled_req as *const timespec, real_rem_ptr) };
            if rc != 0 && !rem.is_null() {
                let real_remaining_ns = timespec_to_ns(&scaled_rem);
                let virt_remaining_ns = if state.scale_sleep && state.speed > 0.0 {
                    (real_remaining_ns as f64 * state.speed) as i128
                } else {
                    real_remaining_ns
                };
                unsafe {
                    *rem = ns_to_timespec(virt_remaining_ns);
                }
            }
            rc
        },
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn usleep(usec: useconds_t) -> c_int {
    let Some(real) = *REAL_USLEEP else {
        return -1;
    };
    with_hook_guard(
        || unsafe { real(usec) },
        || {
            let state = read_state();
            let req_ns = (usec as i128) * 1_000;
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            let scaled_us = scaled_ns.div_euclid(1_000) as useconds_t;
            unsafe { real(scaled_us) }
        },
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sleep(seconds: libc::c_uint) -> libc::c_uint {
    let Some(real) = *REAL_SLEEP else {
        return seconds;
    };
    with_hook_guard(
        || unsafe { real(seconds) },
        || {
            let state = read_state();
            let req_ns = (seconds as i128) * 1_000_000_000;
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            let scaled_sec = scaled_ns.div_euclid(1_000_000_000) as libc::c_uint;
            unsafe { real(scaled_sec) }
        },
    )
}
