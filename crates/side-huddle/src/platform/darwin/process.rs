    use libc::{c_char, c_uint, c_int};

    extern "C" {
        fn proc_name(pid: c_int, buffer: *mut c_char, buffersize: c_uint) -> c_int;
    }

    pub(crate) fn proc_name_for_pid(pid: u32) -> String {
        let mut buf = vec![0u8; 1024];
        let sz = unsafe { proc_name(pid as c_int, buf.as_mut_ptr() as *mut c_char, buf.len() as c_uint) };
        if sz <= 0 { return String::new(); }
        String::from_utf8_lossy(&buf[..sz as usize]).trim_end_matches('\0').to_string()
    }
    