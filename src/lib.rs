use libc::{
    c_char, c_int, clockid_t, fd_set, itimerval, nfds_t, pollfd, sigset_t, time_t, timespec,
    timeval, useconds_t,
};
use once_cell::sync::Lazy;
use std::cell::Cell;
use std::env;
use std::ffi::CString;
use std::sync::atomic::{AtomicU64, Ordering};

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
static DEBUG_HOOKS: Lazy<bool> = Lazy::new(|| {
    env::var("TW_DEBUG_HOOKS")
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
});
static DEBUG_SEQ: AtomicU64 = AtomicU64::new(1);

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
static REAL_ALARM: Lazy<Option<unsafe extern "C" fn(libc::c_uint) -> libc::c_uint>> =
    Lazy::new(|| unsafe { resolve_symbol("alarm") });
static REAL_UALARM: Lazy<Option<unsafe extern "C" fn(useconds_t, useconds_t) -> useconds_t>> =
    Lazy::new(|| unsafe { resolve_symbol("ualarm") });
static REAL_CLOCK_NANOSLEEP: Lazy<
    Option<unsafe extern "C" fn(clockid_t, c_int, *const timespec, *mut timespec) -> c_int>,
> = Lazy::new(|| unsafe { resolve_symbol("clock_nanosleep") });
static REAL_SELECT: Lazy<
    Option<
        unsafe extern "C" fn(
            c_int,
            *mut fd_set,
            *mut fd_set,
            *mut fd_set,
            *mut timeval,
        ) -> c_int,
    >,
> = Lazy::new(|| unsafe { resolve_symbol("select") });
static REAL_PSELECT: Lazy<
    Option<
        unsafe extern "C" fn(
            c_int,
            *mut fd_set,
            *mut fd_set,
            *mut fd_set,
            *const timespec,
            *const sigset_t,
        ) -> c_int,
    >,
> = Lazy::new(|| unsafe { resolve_symbol("pselect") });
static REAL_POLL: Lazy<Option<unsafe extern "C" fn(*mut pollfd, nfds_t, c_int) -> c_int>> =
    Lazy::new(|| unsafe { resolve_symbol("poll") });
static REAL_PPOLL: Lazy<
    Option<unsafe extern "C" fn(*mut pollfd, nfds_t, *const timespec, *const sigset_t) -> c_int>,
> = Lazy::new(|| unsafe { resolve_symbol("ppoll") });
static REAL_SETITIMER: Lazy<
    Option<unsafe extern "C" fn(c_int, *const itimerval, *mut itimerval) -> c_int>,
> = Lazy::new(|| unsafe { resolve_symbol("setitimer") });
#[cfg(target_os = "linux")]
static REAL_TIMER_SETTIME: Lazy<
    Option<unsafe extern "C" fn(libc::timer_t, c_int, *const libc::itimerspec, *mut libc::itimerspec) -> c_int>,
> = Lazy::new(|| unsafe { resolve_symbol("timer_settime") });
#[cfg(target_os = "linux")]
static REAL_TIMERFD_SETTIME: Lazy<
    Option<unsafe extern "C" fn(c_int, c_int, *const libc::itimerspec, *mut libc::itimerspec) -> c_int>,
> = Lazy::new(|| unsafe { resolve_symbol("timerfd_settime") });

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
    state.warp_monotonic
        && (clock_id == libc::CLOCK_MONOTONIC
            || {
                #[cfg(any(target_os = "linux", target_os = "android"))]
                {
                    clock_id == libc::CLOCK_MONOTONIC_RAW
                }
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                {
                    false
                }
            })
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

fn timeval_to_ns(tv: &timeval) -> i128 {
    (tv.tv_sec as i128) * 1_000_000_000 + (tv.tv_usec as i128) * 1_000
}

fn ns_to_timeval(ns: i128) -> timeval {
    let sec = ns.div_euclid(1_000_000_000);
    let usec = ns.rem_euclid(1_000_000_000) / 1_000;
    timeval {
        tv_sec: sec as libc::time_t,
        tv_usec: usec as libc::suseconds_t,
    }
}

fn debug_log(msg: &str) {
    if !*DEBUG_HOOKS {
        return;
    }
    let seq = DEBUG_SEQ.fetch_add(1, Ordering::Relaxed);
    eprintln!("[timewarp-shim#{seq}] {msg}");
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
/// Interposes `clock_gettime` and optionally warps returned time values.
///
/// # Safety
/// `tp` must be either null (invalid for this wrapper) or a valid writable pointer
/// to a `timespec` provided by the caller per libc contract.
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
                debug_log(&format!(
                    "clock_gettime hit clock_id={clock_id} base_ns={base_ns} warped_ns={warped_ns} speed={} warp_monotonic={}",
                    state.speed, state.warp_monotonic
                ));
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

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_gettime64(clock_id: clockid_t, tp: *mut timespec) -> c_int {
    unsafe { clock_gettime(clock_id, tp) }
}

#[unsafe(no_mangle)]
/// Interposes `time` and optionally warps returned wall-clock seconds.
///
/// # Safety
/// If `out` is non-null, it must be a valid writable pointer to `time_t` per libc
/// contract.
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
/// Interposes `gettimeofday` and optionally warps returned wall-clock time.
///
/// # Safety
/// If `tv` is non-null, it must be a valid writable pointer to `timeval` per libc
/// contract. `tz` is forwarded as-is to libc.
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
/// Interposes `nanosleep` and optionally scales requested sleep duration.
///
/// # Safety
/// `req` must be a valid readable pointer to `timespec`. If `rem` is non-null, it
/// must be a valid writable pointer to `timespec` per libc contract.
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
            debug_log(&format!(
                "nanosleep hit req_ns={req_ns} scaled_ns={scaled_ns} scale_sleep={} speed={}",
                state.scale_sleep, state.speed
            ));
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
/// Interposes `usleep` and optionally scales requested sleep duration.
///
/// # Safety
/// Must be called with the same preconditions as libc `usleep`.
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
            debug_log(&format!(
                "usleep hit req_us={usec} scaled_ns={scaled_ns} scale_sleep={} speed={}",
                state.scale_sleep, state.speed
            ));
            let scaled_us = scaled_ns.div_euclid(1_000) as useconds_t;
            unsafe { real(scaled_us) }
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `sleep` and optionally scales requested sleep duration.
///
/// # Safety
/// Must be called with the same preconditions as libc `sleep`.
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
            debug_log(&format!(
                "sleep hit req_s={seconds} scaled_ns={scaled_ns} scale_sleep={} speed={}",
                state.scale_sleep, state.speed
            ));
            let scaled_sec = scaled_ns.div_euclid(1_000_000_000) as libc::c_uint;
            unsafe { real(scaled_sec) }
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `alarm` and optionally scales requested alarm duration.
///
/// # Safety
/// Must be called with the same preconditions as libc `alarm`.
pub unsafe extern "C" fn alarm(seconds: libc::c_uint) -> libc::c_uint {
    let Some(real) = *REAL_ALARM else {
        return seconds;
    };
    with_hook_guard(
        || unsafe { real(seconds) },
        || {
            let state = read_state();
            let req_ns = (seconds as i128) * 1_000_000_000;
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            let scaled_sec = scaled_ns.div_euclid(1_000_000_000) as libc::c_uint;
            debug_log(&format!(
                "alarm hit req_s={seconds} scaled_s={scaled_sec} speed={}",
                state.speed
            ));
            unsafe { real(scaled_sec) }
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `ualarm` and optionally scales requested alarm/interval durations.
///
/// # Safety
/// Must be called with the same preconditions as libc `ualarm`.
pub unsafe extern "C" fn ualarm(value: useconds_t, interval: useconds_t) -> useconds_t {
    let Some(real) = *REAL_UALARM else {
        return 0;
    };
    with_hook_guard(
        || unsafe { real(value, interval) },
        || {
            let state = read_state();
            let value_ns = (value as i128) * 1_000;
            let interval_ns = (interval as i128) * 1_000;
            let scaled_value_ns = scale_sleep_duration_ns(value_ns, state);
            let scaled_interval_ns = scale_sleep_duration_ns(interval_ns, state);
            let scaled_value = scaled_value_ns.div_euclid(1_000) as useconds_t;
            let scaled_interval = scaled_interval_ns.div_euclid(1_000) as useconds_t;
            debug_log(&format!(
                "ualarm hit value_us={value} interval_us={interval} scaled_value_us={scaled_value} scaled_interval_us={scaled_interval} speed={}",
                state.speed
            ));
            unsafe { real(scaled_value, scaled_interval) }
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `clock_nanosleep` and optionally scales requested sleep duration.
///
/// # Safety
/// `request` must be a valid readable pointer to `timespec`. If `remain` is non-null,
/// it must be a valid writable pointer to `timespec` per libc contract.
pub unsafe extern "C" fn clock_nanosleep(
    clock_id: clockid_t,
    flags: c_int,
    request: *const timespec,
    remain: *mut timespec,
) -> c_int {
    let Some(real) = *REAL_CLOCK_NANOSLEEP else {
        return -1;
    };
    if request.is_null() {
        return -1;
    }

    with_hook_guard(
        || unsafe { real(clock_id, flags, request, remain) },
        || {
            let state = read_state();
            // Absolute sleeps should not be scaled here.
            const TIMER_ABSTIME_FLAG: c_int = 1;
            let is_absolute = flags & TIMER_ABSTIME_FLAG != 0;
            if is_absolute {
                debug_log(&format!(
                    "clock_nanosleep hit ABS flags={flags} req_ns={} (unscaled)",
                    unsafe { timespec_to_ns(&*request) }
                ));
                return unsafe { real(clock_id, flags, request, remain) };
            }

            let req_ns = unsafe { timespec_to_ns(&*request) };
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            debug_log(&format!(
                "clock_nanosleep hit clock_id={clock_id} req_ns={req_ns} scaled_ns={scaled_ns} speed={}",
                state.speed
            ));
            let scaled_req = ns_to_timespec(scaled_ns);
            let mut scaled_rem = timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let real_rem_ptr = if remain.is_null() {
                std::ptr::null_mut()
            } else {
                &mut scaled_rem as *mut timespec
            };

            let rc = unsafe { real(clock_id, flags, &scaled_req as *const timespec, real_rem_ptr) };
            if rc != 0 && !remain.is_null() {
                let real_remaining_ns = timespec_to_ns(&scaled_rem);
                let virt_remaining_ns = if state.scale_sleep && state.speed > 0.0 {
                    (real_remaining_ns as f64 * state.speed) as i128
                } else {
                    real_remaining_ns
                };
                unsafe {
                    *remain = ns_to_timespec(virt_remaining_ns);
                }
            }
            rc
        },
    )
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
/// Linux alias for `clock_nanosleep`.
///
/// # Safety
/// Same requirements as [`clock_nanosleep`].
pub unsafe extern "C" fn __clock_nanosleep(
    clock_id: clockid_t,
    flags: c_int,
    request: *const timespec,
    remain: *mut timespec,
) -> c_int {
    unsafe { clock_nanosleep(clock_id, flags, request, remain) }
}

#[unsafe(no_mangle)]
/// Interposes `select` and optionally scales timeout.
///
/// # Safety
/// Pointer arguments must satisfy libc `select` requirements.
pub unsafe extern "C" fn select(
    nfds: c_int,
    readfds: *mut fd_set,
    writefds: *mut fd_set,
    exceptfds: *mut fd_set,
    timeout: *mut timeval,
) -> c_int {
    let Some(real) = *REAL_SELECT else {
        return -1;
    };

    with_hook_guard(
        || unsafe { real(nfds, readfds, writefds, exceptfds, timeout) },
        || {
            if timeout.is_null() {
                return unsafe { real(nfds, readfds, writefds, exceptfds, timeout) };
            }

            let state = read_state();
            let req_ns = unsafe { timeval_to_ns(&*timeout) };
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            debug_log(&format!(
                "select hit req_ns={req_ns} scaled_ns={scaled_ns} speed={}",
                state.speed
            ));
            let mut scaled_tv = ns_to_timeval(scaled_ns);
            let rc = unsafe {
                real(
                    nfds,
                    readfds,
                    writefds,
                    exceptfds,
                    &mut scaled_tv as *mut timeval,
                )
            };

            if rc != 0 {
                let real_remaining_ns = timeval_to_ns(&scaled_tv);
                let virt_remaining_ns = if state.scale_sleep && state.speed > 0.0 {
                    (real_remaining_ns as f64 * state.speed) as i128
                } else {
                    real_remaining_ns
                };
                unsafe {
                    *timeout = ns_to_timeval(virt_remaining_ns);
                }
            }
            rc
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `pselect` and optionally scales timeout.
///
/// # Safety
/// Pointer arguments must satisfy libc `pselect` requirements.
pub unsafe extern "C" fn pselect(
    nfds: c_int,
    readfds: *mut fd_set,
    writefds: *mut fd_set,
    exceptfds: *mut fd_set,
    timeout: *const timespec,
    sigmask: *const sigset_t,
) -> c_int {
    let Some(real) = *REAL_PSELECT else {
        return -1;
    };

    with_hook_guard(
        || unsafe { real(nfds, readfds, writefds, exceptfds, timeout, sigmask) },
        || {
            if timeout.is_null() {
                return unsafe { real(nfds, readfds, writefds, exceptfds, timeout, sigmask) };
            }

            let state = read_state();
            let req_ns = unsafe { timespec_to_ns(&*timeout) };
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            debug_log(&format!(
                "pselect hit req_ns={req_ns} scaled_ns={scaled_ns} speed={}",
                state.speed
            ));
            let scaled_ts = ns_to_timespec(scaled_ns);
            unsafe {
                real(
                    nfds,
                    readfds,
                    writefds,
                    exceptfds,
                    &scaled_ts as *const timespec,
                    sigmask,
                )
            }
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `poll` and optionally scales timeout.
///
/// # Safety
/// Pointer arguments must satisfy libc `poll` requirements.
pub unsafe extern "C" fn poll(fds: *mut pollfd, nfds: nfds_t, timeout_ms: c_int) -> c_int {
    let Some(real) = *REAL_POLL else {
        return -1;
    };

    with_hook_guard(
        || unsafe { real(fds, nfds, timeout_ms) },
        || {
            if timeout_ms < 0 {
                return unsafe { real(fds, nfds, timeout_ms) };
            }
            let state = read_state();
            let req_ns = (timeout_ms as i128) * 1_000_000;
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            debug_log(&format!(
                "poll hit req_ms={timeout_ms} scaled_ns={scaled_ns} speed={}",
                state.speed
            ));
            let scaled_ms = scaled_ns.div_euclid(1_000_000) as c_int;
            unsafe { real(fds, nfds, scaled_ms) }
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `ppoll` and optionally scales timeout.
///
/// # Safety
/// Pointer arguments must satisfy libc `ppoll` requirements.
pub unsafe extern "C" fn ppoll(
    fds: *mut pollfd,
    nfds: nfds_t,
    timeout: *const timespec,
    sigmask: *const sigset_t,
) -> c_int {
    let Some(real) = *REAL_PPOLL else {
        return -1;
    };

    with_hook_guard(
        || unsafe { real(fds, nfds, timeout, sigmask) },
        || {
            if timeout.is_null() {
                return unsafe { real(fds, nfds, timeout, sigmask) };
            }
            let state = read_state();
            let req_ns = unsafe { timespec_to_ns(&*timeout) };
            let scaled_ns = scale_sleep_duration_ns(req_ns, state);
            debug_log(&format!(
                "ppoll hit req_ns={req_ns} scaled_ns={scaled_ns} speed={}",
                state.speed
            ));
            let scaled_ts = ns_to_timespec(scaled_ns);
            unsafe { real(fds, nfds, &scaled_ts as *const timespec, sigmask) }
        },
    )
}

#[unsafe(no_mangle)]
/// Interposes `setitimer` and optionally scales interval/initial timer values.
///
/// # Safety
/// Pointer arguments must satisfy libc `setitimer` requirements.
pub unsafe extern "C" fn setitimer(
    which: c_int,
    new_value: *const itimerval,
    old_value: *mut itimerval,
) -> c_int {
    let Some(real) = *REAL_SETITIMER else {
        return -1;
    };
    with_hook_guard(
        || unsafe { real(which, new_value, old_value) },
        || {
            if new_value.is_null() {
                return unsafe { real(which, new_value, old_value) };
            }
            let state = read_state();
            let newv = unsafe { *new_value };
            let scaled_interval_ns = scale_sleep_duration_ns(timeval_to_ns(&newv.it_interval), state);
            let scaled_value_ns = scale_sleep_duration_ns(timeval_to_ns(&newv.it_value), state);
            debug_log(&format!(
                "setitimer hit value_ns={} interval_ns={} scaled_value_ns={} scaled_interval_ns={} speed={}",
                timeval_to_ns(&newv.it_value),
                timeval_to_ns(&newv.it_interval),
                scaled_value_ns,
                scaled_interval_ns,
                state.speed
            ));
            let scaled = itimerval {
                it_interval: ns_to_timeval(scaled_interval_ns),
                it_value: ns_to_timeval(scaled_value_ns),
            };
            unsafe { real(which, &scaled as *const itimerval, old_value) }
        },
    )
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
/// Interposes `timer_settime` and optionally scales relative timer values.
///
/// # Safety
/// Pointer arguments must satisfy libc `timer_settime` requirements.
pub unsafe extern "C" fn timer_settime(
    timerid: libc::timer_t,
    flags: c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> c_int {
    let Some(real) = *REAL_TIMER_SETTIME else {
        return -1;
    };
    with_hook_guard(
        || unsafe { real(timerid, flags, new_value, old_value) },
        || {
            if new_value.is_null() {
                return unsafe { real(timerid, flags, new_value, old_value) };
            }
            const TIMER_ABSTIME_FLAG: c_int = 1;
            if flags & TIMER_ABSTIME_FLAG != 0 {
                // Absolute deadlines should not be scaled here.
                return unsafe { real(timerid, flags, new_value, old_value) };
            }
            let state = read_state();
            let newv = unsafe { *new_value };
            let scaled_interval_ns = scale_sleep_duration_ns(timespec_to_ns(&newv.it_interval), state);
            let scaled_value_ns = scale_sleep_duration_ns(timespec_to_ns(&newv.it_value), state);
            debug_log(&format!(
                "timer_settime hit value_ns={} interval_ns={} scaled_value_ns={} scaled_interval_ns={} speed={}",
                timespec_to_ns(&newv.it_value),
                timespec_to_ns(&newv.it_interval),
                scaled_value_ns,
                scaled_interval_ns,
                state.speed
            ));
            let scaled = libc::itimerspec {
                it_interval: ns_to_timespec(scaled_interval_ns),
                it_value: ns_to_timespec(scaled_value_ns),
            };
            unsafe { real(timerid, flags, &scaled as *const libc::itimerspec, old_value) }
        },
    )
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
/// Linux alias for `timer_settime`.
///
/// # Safety
/// Same requirements as [`timer_settime`].
pub unsafe extern "C" fn __timer_settime(
    timerid: libc::timer_t,
    flags: c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> c_int {
    unsafe { timer_settime(timerid, flags, new_value, old_value) }
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
/// Linux alias for `setitimer`.
///
/// # Safety
/// Same requirements as [`setitimer`].
pub unsafe extern "C" fn __setitimer(
    which: c_int,
    new_value: *const itimerval,
    old_value: *mut itimerval,
) -> c_int {
    unsafe { setitimer(which, new_value, old_value) }
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
/// Interposes `timerfd_settime` and optionally scales relative timer values.
///
/// # Safety
/// Pointer arguments must satisfy libc `timerfd_settime` requirements.
pub unsafe extern "C" fn timerfd_settime(
    fd: c_int,
    flags: c_int,
    new_value: *const libc::itimerspec,
    old_value: *mut libc::itimerspec,
) -> c_int {
    let Some(real) = *REAL_TIMERFD_SETTIME else {
        return -1;
    };
    with_hook_guard(
        || unsafe { real(fd, flags, new_value, old_value) },
        || {
            if new_value.is_null() {
                return unsafe { real(fd, flags, new_value, old_value) };
            }
            const TFD_TIMER_ABSTIME_FLAG: c_int = 1;
            if flags & TFD_TIMER_ABSTIME_FLAG != 0 {
                // Absolute deadlines should not be scaled here.
                debug_log(&format!(
                    "timerfd_settime hit ABS flags={flags} value_ns={} (unscaled)",
                    unsafe { timespec_to_ns(&(*new_value).it_value) }
                ));
                return unsafe { real(fd, flags, new_value, old_value) };
            }
            let state = read_state();
            let newv = unsafe { *new_value };
            let scaled_interval_ns = scale_sleep_duration_ns(timespec_to_ns(&newv.it_interval), state);
            let scaled_value_ns = scale_sleep_duration_ns(timespec_to_ns(&newv.it_value), state);
            debug_log(&format!(
                "timerfd_settime hit value_ns={} interval_ns={} scaled_value_ns={} scaled_interval_ns={} speed={}",
                timespec_to_ns(&newv.it_value),
                timespec_to_ns(&newv.it_interval),
                scaled_value_ns,
                scaled_interval_ns,
                state.speed
            ));
            let scaled = libc::itimerspec {
                it_interval: ns_to_timespec(scaled_interval_ns),
                it_value: ns_to_timespec(scaled_value_ns),
            };
            unsafe { real(fd, flags, &scaled as *const libc::itimerspec, old_value) }
        },
    )
}
