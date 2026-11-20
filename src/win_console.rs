//! Windows 控制台附着（V3.15）：release 构建是 GUI 子系统（双击不弹
//! 终端），CLI 子命令在 main 开头 AttachConsole 挂回调用方终端并重开
//! 标准句柄，list/test 等输出照常可见。纯 FFI，不引依赖。

const ATTACH_PARENT_PROCESS: usize = usize::MAX;
const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // -11
const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // -12
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_WRITE: u32 = 0x2;
const OPEN_EXISTING: u32 = 3;

#[link(name = "kernel32")]
extern "system" {
    fn AttachConsole(dwProcessId: usize) -> i32;
    fn CreateFileA(
        lpFileName: *const u8,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *mut core::ffi::c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void;
    fn SetStdHandle(nStdHandle: u32, hHandle: *mut core::ffi::c_void) -> i32;
}

/// 挂回父进程终端（从终端里跑 CLI 时有效；双击/无终端场景静默跳过）。
/// AttachConsole 成功后进程的标准句柄仍指向无效值，须重开 CONOUT$ 并
/// SetStdHandle——Rust std 在首次使用 stdout/stderr 时取句柄，所以本
/// 函数必须在任何输出之前调用。
pub fn attach_parent() {
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return; // 无父终端（资源管理器双击等）
        }
        let name = b"CONOUT$\0";
        let h = CreateFileA(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_WRITE,
            core::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            core::ptr::null_mut(),
        );
        if !h.is_null() && h as isize != -1 {
            SetStdHandle(STD_OUTPUT_HANDLE, h);
            SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
}
