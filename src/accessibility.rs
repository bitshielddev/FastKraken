#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Display {
    pub(crate) contrast: bool,
    pub(crate) reduce_motion: bool,
}

pub(crate) fn observed() -> Option<Display> {
    if cfg!(test) || crate::acceptance::replaying() {
        return None;
    }
    Some(read())
}

#[cfg(target_os = "macos")]
fn read() -> Display {
    use std::ffi::{CStr, c_char, c_void};

    type Object = *mut c_void;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Object;
        fn sel_registerName(name: *const c_char) -> Object;
        fn objc_msgSend();
    }

    unsafe {
        let send_object: unsafe extern "C" fn(Object, Object) -> Object =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_flag: unsafe extern "C" fn(Object, Object) -> i8 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let class = objc_getClass(c"NSWorkspace".as_ptr());
        if class.is_null() {
            return Display::default();
        }
        let workspace = send_object(class, sel_registerName(c"sharedWorkspace".as_ptr()));
        if workspace.is_null() {
            return Display::default();
        }
        let flag = |name: &CStr| send_flag(workspace, sel_registerName(name.as_ptr())) != 0;
        Display {
            contrast: flag(c"accessibilityDisplayShouldIncreaseContrast"),
            reduce_motion: flag(c"accessibilityDisplayShouldReduceMotion"),
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn read() -> Display {
    Display::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_reads_the_system_display_settings_only_outside_tests_and_replays() {
        assert_eq!(observed(), None);
        assert_eq!(Display::default(), Display { contrast: false, reduce_motion: false });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_display_settings_can_be_read_without_changing_them() {
        let first = read();
        assert_eq!(read(), first);
    }
}
