use crate::appearance::Dates;

const SINCE_2001: f64 = 978_307_200.;

pub(crate) fn in_graph(time: i64, date: &str, now: i64, dates: Dates) -> String {
    match dates {
        Dates::Absolute if time > 0 => short(time),
        _ => crate::view::when(time, date, now),
    }
}

#[cfg(target_os = "macos")]
mod system {
    use std::{
        cell::RefCell,
        ffi::{c_char, c_void},
    };

    type Ref = *const c_void;

    const SHORT_STYLE: isize = 1;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFLocaleCopyCurrent() -> Ref;
        fn CFDateFormatterCreate(allocator: Ref, locale: Ref, date_style: isize, time_style: isize) -> Ref;
        fn CFDateFormatterCreateStringWithAbsoluteTime(allocator: Ref, formatter: Ref, at: f64) -> Ref;
        fn CFStringGetLength(string: Ref) -> isize;
        fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        fn CFStringGetCString(string: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(object: Ref);
    }

    struct Formatter(Ref);

    impl Drop for Formatter {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }

    fn create() -> Option<Formatter> {
        unsafe {
            let locale = CFLocaleCopyCurrent();
            if locale.is_null() {
                return None;
            }
            let formatter = CFDateFormatterCreate(std::ptr::null(), locale, SHORT_STYLE, SHORT_STYLE);
            CFRelease(locale);
            (!formatter.is_null()).then_some(Formatter(formatter))
        }
    }

    fn text(string: Ref) -> Option<String> {
        unsafe {
            let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(string), UTF8) + 1;
            let mut buffer = vec![0u8; usize::try_from(size).ok()?];
            if CFStringGetCString(string, buffer.as_mut_ptr().cast(), size, UTF8) == 0 {
                return None;
            }
            let end = buffer.iter().position(|byte| *byte == 0)?;
            buffer.truncate(end);
            String::from_utf8(buffer).ok()
        }
    }

    thread_local! {
        static FORMATTER: RefCell<Option<Formatter>> = const { RefCell::new(None) };
    }

    pub(super) fn short(time: i64) -> Option<String> {
        FORMATTER.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.is_none() {
                *cell = create();
            }
            let formatter = cell.as_ref()?;
            unsafe {
                let string = CFDateFormatterCreateStringWithAbsoluteTime(std::ptr::null(), formatter.0, time as f64 - super::SINCE_2001);
                if string.is_null() {
                    return None;
                }
                let shown = text(string);
                CFRelease(string);
                shown
            }
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod system {
    pub(super) fn short(time: i64) -> Option<String> {
        let seconds = time as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
            return None;
        }
        Some(format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min))
    }
}

pub(crate) fn short(time: i64) -> String {
    system::short(time).unwrap_or_else(|| crate::view::date_of(time))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(time: i64) -> libc::tm {
        let seconds = time as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        assert!(!unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null());
        tm
    }

    #[test]
    fn absolute_dates_carry_the_day_the_year_and_the_minute_and_relative_ones_stay() {
        let now = 1_795_000_000;
        let time = now - 3 * 3_600;
        let tm = local(time);
        let shown = in_graph(time, &crate::view::date_of(time), now, Dates::Absolute);
        assert!(!shown.contains("ago"), "{shown}");
        assert!(shown.contains(&format!("{:02}", tm.tm_min)), "{shown} has the minute");
        assert!(shown.contains(&tm.tm_mday.to_string()), "{shown} has the day");
        let year = (tm.tm_year + 1900).to_string();
        assert!(shown.contains(&year) || shown.contains(&year[2..]), "{shown} has the year");
        assert_eq!(shown, short(time));
        assert_eq!(in_graph(time, "2026-11-18", now, Dates::Relative), "3 hours ago");
        assert_eq!(in_graph(0, "2026-11-18", now, Dates::Absolute), crate::view::when(0, "2026-11-18", now));
        let earlier = short(time - 86_400 * 400);
        assert_ne!(earlier, shown);
    }
}
