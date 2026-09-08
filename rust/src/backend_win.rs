//! Windows transport: the stock CYUSB3 driver's IOCTLs, straight through
//! DeviceIoControl. Declared inline rather than pulled from a binding crate,
//! so the core builds with no dependencies at all.
//!
//! The interface GUID is not a constant: it comes from whichever .inf bound the
//! board, which is why it gets read out of the device's own registry key.

#![allow(non_snake_case, non_camel_case_types)]

use crate::usb::{BoardError, Result, Transport, Xfer};

type HANDLE = isize;
type BOOL = i32;
type DWORD = u32;

const INVALID_HANDLE: HANDLE = -1;
const GENERIC_READ: DWORD = 0x8000_0000;
const GENERIC_WRITE: DWORD = 0x4000_0000;
const FILE_SHARE_RW: DWORD = 0x03;
const OPEN_EXISTING: DWORD = 3;
const FILE_FLAG_OVERLAPPED: DWORD = 0x4000_0000;
const ERROR_IO_PENDING: DWORD = 997;
const WAIT_OBJECT_0: DWORD = 0;

const DIGCF_PRESENT: DWORD = 0x02;
const DIGCF_DEVICEINTERFACE: DWORD = 0x10;

const IOCTL_NON_EP0_XFER: DWORD = 0x0022_0024;
const IOCTL_RESET_PIPE: DWORD = 0x0022_002C;
const IOCTL_ABORT_PIPE: DWORD = 0x0022_0044;

/// Header ahead of the payload in the CYUSB3 transfer buffer.
const HDR: usize = 0x26;

const CYUSBDRV_GUID: &str = "{AE18AA60-7F6A-11D4-97DD-00010229B959}";
const HKEY_LOCAL_MACHINE: HANDLE = -2147483646; // 0x80000002

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl GUID {
    /// Parse "{AE18AA60-7F6A-11D4-97DD-00010229B959}".
    fn parse(s: &str) -> Option<GUID> {
        let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if h.len() != 32 {
            return None;
        }
        let b = |i: usize, n: usize| u64::from_str_radix(&h[i..i + n], 16).ok();
        let mut data4 = [0u8; 8];
        for i in 0..8 {
            data4[i] = b(16 + i * 2, 2)? as u8;
        }
        Some(GUID {
            data1: b(0, 8)? as u32,
            data2: b(8, 4)? as u16,
            data3: b(12, 4)? as u16,
            data4,
        })
    }
}

#[repr(C)]
struct OVERLAPPED {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: HANDLE,
}

#[repr(C)]
struct SP_DEVICE_INTERFACE_DATA {
    cb_size: DWORD,
    interface_class_guid: GUID,
    flags: DWORD,
    reserved: usize,
}

extern "system" {
    fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *const core::ffi::c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    fn CloseHandle(h: HANDLE) -> BOOL;
    fn CreateEventW(
        attrs: *const core::ffi::c_void,
        manual_reset: BOOL,
        initial: BOOL,
        name: *const u16,
    ) -> HANDLE;
    fn WaitForSingleObject(h: HANDLE, ms: DWORD) -> DWORD;
    fn CancelIo(h: HANDLE) -> BOOL;
    fn GetOverlappedResult(h: HANDLE, ov: *mut OVERLAPPED, moved: *mut DWORD, wait: BOOL) -> BOOL;
    fn GetLastError() -> DWORD;
    fn DeviceIoControl(
        h: HANDLE,
        code: DWORD,
        inbuf: *mut u8,
        insize: DWORD,
        outbuf: *mut u8,
        outsize: DWORD,
        returned: *mut DWORD,
        ov: *mut OVERLAPPED,
    ) -> BOOL;

}

// SetupAPI and the registry live in their own libraries; kernel32 is linked by
// default, these two are not.
#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(
        guid: *const GUID,
        enumerator: *const u16,
        parent: HANDLE,
        flags: DWORD,
    ) -> HANDLE;
    fn SetupDiEnumDeviceInterfaces(
        set: HANDLE,
        info: *const core::ffi::c_void,
        guid: *const GUID,
        index: DWORD,
        data: *mut SP_DEVICE_INTERFACE_DATA,
    ) -> BOOL;
    fn SetupDiGetDeviceInterfaceDetailW(
        set: HANDLE,
        iface: *mut SP_DEVICE_INTERFACE_DATA,
        detail: *mut u8,
        detail_size: DWORD,
        required: *mut DWORD,
        info: *mut core::ffi::c_void,
    ) -> BOOL;
    fn SetupDiDestroyDeviceInfoList(set: HANDLE) -> BOOL;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        key: HANDLE,
        sub: *const u16,
        options: DWORD,
        desired: DWORD,
        out: *mut HANDLE,
    ) -> i32;
    fn RegEnumKeyExW(
        key: HANDLE,
        index: DWORD,
        name: *mut u16,
        name_len: *mut DWORD,
        reserved: *mut DWORD,
        class: *mut u16,
        class_len: *mut DWORD,
        write_time: *mut core::ffi::c_void,
    ) -> i32;
    fn RegQueryValueExW(
        key: HANDLE,
        name: *const u16,
        reserved: *mut DWORD,
        kind: *mut DWORD,
        data: *mut u8,
        size: *mut DWORD,
    ) -> i32;
    fn RegCloseKey(key: HANDLE) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

const KEY_READ: DWORD = 0x2_0019;

fn reg_open(parent: HANDLE, sub: &str) -> Option<HANDLE> {
    let mut h: HANDLE = 0;
    let rc = unsafe { RegOpenKeyExW(parent, wide(sub).as_ptr(), 0, KEY_READ, &mut h) };
    if rc == 0 {
        Some(h)
    } else {
        None
    }
}

fn reg_strings(key: HANDLE, name: &str) -> Vec<String> {
    let mut size: DWORD = 0;
    let n = wide(name);
    let rc = unsafe {
        RegQueryValueExW(
            key,
            n.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc != 0 || size == 0 {
        return vec![];
    }
    let mut buf = vec![0u8; size as usize + 2];
    let rc = unsafe {
        RegQueryValueExW(
            key,
            n.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    if rc != 0 {
        return vec![];
    }
    // REG_SZ or REG_MULTI_SZ: split on NULs either way.
    let units: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    units
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf16_lossy(s))
        .collect()
}

/// Interface GUIDs registered for this VID/PID, CYUSB3 instances first.
fn registry_guids() -> Vec<String> {
    let path = format!(
        "SYSTEM\\CurrentControlSet\\Enum\\USB\\VID_{:04X}&PID_{:04X}",
        crate::usb::VID,
        crate::usb::PID
    );
    let root = match reg_open(HKEY_LOCAL_MACHINE, &path) {
        Some(h) => h,
        None => return vec![],
    };
    let (mut cy, mut other) = (Vec::new(), Vec::new());
    let mut index: DWORD = 0;
    loop {
        let mut name = [0u16; 512];
        let mut len: DWORD = name.len() as DWORD;
        let rc = unsafe {
            RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            break;
        }
        index += 1;
        let inst = from_wide(&name);
        let mut service = String::new();
        if let Some(k) = reg_open(root, &inst) {
            if let Some(s) = reg_strings(k, "Service").into_iter().next() {
                service = s;
            }
            unsafe { RegCloseKey(k) };
        }
        let mut found = Vec::new();
        if let Some(dp) = reg_open(root, &format!("{}\\Device Parameters", inst)) {
            for value in ["DeviceInterfaceGUIDs", "DeviceInterfaceGUID", "DriverGUID"] {
                found.extend(reg_strings(dp, value));
            }
            unsafe { RegCloseKey(dp) };
        }
        let up = service.to_ascii_uppercase();
        let bucket = if up == "CYUSB3" || up == "CYUSB" {
            &mut cy
        } else {
            &mut other
        };
        for g in found {
            let g = g.trim_matches(|c| c == '{' || c == '}' || c == ' ').to_string();
            if !g.is_empty() {
                bucket.push(format!("{{{}}}", g));
            }
        }
    }
    unsafe { RegCloseKey(root) };
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for g in cy.into_iter().chain(other) {
        let low = g.to_ascii_lowercase();
        if !seen.contains(&low) {
            seen.push(low);
            out.push(g);
        }
    }
    out
}

fn paths_for_guid(guid_str: &str) -> Vec<String> {
    let guid = match GUID::parse(guid_str) {
        Some(g) => g,
        None => return vec![],
    };
    let set = unsafe {
        SetupDiGetClassDevsW(
            &guid,
            std::ptr::null(),
            0,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE {
        return vec![];
    }
    let mut out = Vec::new();
    let mut index: DWORD = 0;
    loop {
        let mut did = SP_DEVICE_INTERFACE_DATA {
            cb_size: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as DWORD,
            interface_class_guid: GUID::default(),
            flags: 0,
            reserved: 0,
        };
        let ok = unsafe {
            SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &guid, index, &mut did)
        };
        if ok == 0 {
            break;
        }
        index += 1;
        let mut need: DWORD = 0;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut did,
                std::ptr::null_mut(),
                0,
                &mut need,
                std::ptr::null_mut(),
            )
        };
        if need == 0 {
            continue;
        }
        let mut buf = vec![0u8; need as usize];
        // cbSize of SP_DEVICE_INTERFACE_DETAIL_DATA_W: 8 on 64-bit, 6 on 32-bit.
        let cb: u32 = if std::mem::size_of::<usize>() == 8 { 8 } else { 6 };
        buf[..4].copy_from_slice(&cb.to_le_bytes());
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut did,
                buf.as_mut_ptr(),
                need,
                &mut need,
                std::ptr::null_mut(),
            )
        };
        if ok != 0 {
            let units: Vec<u16> = buf[4..]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            out.push(from_wide(&units));
        }
    }
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    out
}

/// Every DBK2JP interface path on this machine.
///
/// Tries the GUIDs the registry says this VID/PID is bound to, then Cypress's
/// stock one, and keeps only paths carrying our VID/PID: a machine can hold
/// several CyUSB devices, and taking the first one enumerated is a coin flip.
pub fn find_devices(any_vidpid: bool) -> Vec<String> {
    let hwid = format!(
        "vid_{:04x}&pid_{:04x}",
        crate::usb::VID,
        crate::usb::PID
    );
    let mut out: Vec<String> = Vec::new();
    let mut guids = registry_guids();
    guids.push(CYUSBDRV_GUID.to_string());
    for guid in guids {
        for p in paths_for_guid(&guid) {
            let low = p.to_ascii_lowercase();
            if out.iter().any(|q: &String| q.to_ascii_lowercase() == low) {
                continue;
            }
            if any_vidpid || low.contains(&hwid) {
                out.push(p);
            }
        }
    }
    out
}

pub struct WinTransport {
    h: HANDLE,
    path: String,
}

impl WinTransport {
    pub fn open(path: Option<&str>, index: usize) -> Result<Self> {
        let path = match path {
            Some(p) => p.to_string(),
            None => {
                let paths = find_devices(false);
                if paths.is_empty() {
                    return Err(BoardError::NotFound(format!(
                        "VID {:04X} PID {:04X}: unplugged, or bound to a driver other than CYUSB3",
                        crate::usb::VID,
                        crate::usb::PID
                    )));
                }
                if index >= paths.len() {
                    return Err(BoardError::NotFound(format!(
                        "only {} board(s) present",
                        paths.len()
                    )));
                }
                paths[index].clone()
            }
        };
        let h = unsafe {
            CreateFileW(
                wide(&path).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_RW,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                0,
            )
        };
        if h == INVALID_HANDLE {
            return Err(BoardError::Open(format!(
                "{}: Windows error {}",
                path,
                unsafe { GetLastError() }
            )));
        }
        Ok(WinTransport { h, path })
    }

    fn ioctl(
        &mut self,
        code: DWORD,
        buf: &mut [u8],
        insize: DWORD,
        outsize: DWORD,
        timeout_ms: u32,
    ) -> Result<u32> {
        let ev = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        let mut ov = OVERLAPPED {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            h_event: ev,
        };
        let mut got: DWORD = 0;
        let ptr = buf.as_mut_ptr();
        let ok = unsafe {
            DeviceIoControl(
                self.h,
                code,
                ptr,
                insize,
                if outsize > 0 { ptr } else { std::ptr::null_mut() },
                outsize,
                &mut got,
                &mut ov,
            )
        };
        let result = if ok == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_IO_PENDING {
                Err(BoardError::Transfer(format!("Windows error {}", err)))
            } else if unsafe { WaitForSingleObject(ev, timeout_ms) } != WAIT_OBJECT_0 {
                unsafe { CancelIo(self.h) };
                Err(BoardError::Timeout(format!("{} ms", timeout_ms)))
            } else if unsafe { GetOverlappedResult(self.h, &mut ov, &mut got, 0) } == 0 {
                Err(BoardError::Transfer(format!("Windows error {}", unsafe {
                    GetLastError()
                })))
            } else {
                Ok(got)
            }
        } else {
            Ok(got)
        };
        unsafe { CloseHandle(ev) };
        result
    }

    /// Abort and reset take a 1-byte endpoint address. The handle is
    /// overlapped, so these must be issued overlapped too.
    fn pipe_ioctl(&mut self, code: DWORD, ep: u8) -> bool {
        let mut b = [ep];
        self.ioctl(code, &mut b, 1, 0, 1000).is_ok()
    }
}

impl Transport for WinTransport {
    fn path(&self) -> String {
        self.path.clone()
    }

    fn xfer(&mut self, ep: u8, payload: &[u8], read_len: usize, timeout_ms: u32) -> Result<Xfer> {
        let n = if read_len > 0 { read_len } else { payload.len() };
        let mut buf = vec![0u8; HDR + n];
        buf[0x0D] = ep;
        buf[0x1E..0x22].copy_from_slice(&(HDR as u32).to_le_bytes());
        buf[0x22..0x26].copy_from_slice(&(n as u32).to_le_bytes());
        if !payload.is_empty() {
            buf[HDR..HDR + payload.len()].copy_from_slice(payload);
        }
        let total = (HDR + n) as DWORD;
        let got = self.ioctl(IOCTL_NON_EP0_XFER, &mut buf, total, total, timeout_ms)?;
        let nt = u32::from_le_bytes([buf[0x0E], buf[0x0F], buf[0x10], buf[0x11]]);
        let usbd = u32::from_le_bytes([buf[0x12], buf[0x13], buf[0x14], buf[0x15]]);
        Ok(Xfer {
            nt,
            usbd,
            data: buf[HDR..HDR + n].to_vec(),
            moved: (got as usize).saturating_sub(HDR),
        })
    }

    fn abort_pipe(&mut self, ep: u8) -> bool {
        self.pipe_ioctl(IOCTL_ABORT_PIPE, ep)
    }

    fn reset_pipe(&mut self, ep: u8) -> bool {
        self.pipe_ioctl(IOCTL_RESET_PIPE, ep)
    }

    fn close(&mut self) {
        if self.h != 0 && self.h != INVALID_HANDLE {
            unsafe { CloseHandle(self.h) };
            self.h = INVALID_HANDLE;
        }
    }
}

impl Drop for WinTransport {
    fn drop(&mut self) {
        // A script that never closes otherwise leaks the handle until the
        // process exits, and a second open on the same path then fights it.
        self.close();
    }
}
