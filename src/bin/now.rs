fn main() {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };

        libc::gettimeofday(&mut tv as *mut libc::timeval, std::ptr::null_mut());
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts as *mut libc::timespec);

        println!("time={}", t);
        println!("gettimeofday={}", tv.tv_sec);
        println!("clock_gettime={}", ts.tv_sec);

        if let Some(dt) = chrono::DateTime::<chrono::Utc>::from_timestamp(t, 0) {
            println!("formatted_utc={}", dt.format("%Y-%m-%d %H:%M:%S UTC"));
        } else {
            println!("formatted_utc=<out-of-range>");
        }
    }
}
