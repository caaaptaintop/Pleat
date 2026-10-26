//! Windows 原生 System config 位置查询；作者：admin。
//! 父模块须用 #[cfg(windows)] mod codex_windows 声明；本文件也独立限于 Windows。
//! 只返回 Known Folder ProgramData 下的路径，不读取/创建目录或配置文件。
#![cfg(windows)]

use std::{
    ffi::{OsString, c_void},
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    ptr,
};

// 扫描含 NUL 最多 16K 个 u16；转换前已限界，不构造 MAX 长度的借用切片。
const MAX_FOLDER_UNITS: usize = 16 * 1024;
// 与配置来源层的 16 KiB 路径量级一致，包括固定后缀。
const MAX_CONFIG_PATH_BYTES: usize = 16 * 1024;
const CONFIG_SUFFIX: &str = r"OpenAI\Codex\config.toml";
// 仅询问注册位置，不核验目录存在；没有 CREATE / INIT / DEFAULT_PATH 标志。
const KF_FLAG_DONT_VERIFY: u32 = 0x0000_4000;
const S_OK: i32 = 0;

// guiddef.h: unsigned long/u16/u16/unsigned char[8]；Windows long 始终 32 位。
#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const _: [(); 16] = [(); std::mem::size_of::<Guid>()];
const _: [(); 4] = [(); std::mem::align_of::<Guid>()];

// FOLDERID_ProgramData = {62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}
// https://learn.microsoft.com/en-us/windows/win32/shell/knownfolderid
const FOLDERID_PROGRAM_DATA: Guid = Guid {
    data1: 0x62ab_5d82,
    data2: 0xfdc1,
    data3: 0x4dc3,
    data4: [0xa9, 0xdd, 0x07, 0x0d, 0x1d, 0x49, 0x5d, 0x97],
};

// REFKNOWNFOLDERID = const GUID*；DWORD = u32；HANDLE = void*；PWSTR* = u16**。
// HRESULT = LONG = i32；extern "system" 同时保留 x86 stdcall 与 x64/ARM64 ABI。
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetKnownFolderPath(
        folder: *const Guid,
        flags: u32,
        token: *mut c_void,
        path: *mut *mut u16,
    ) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(memory: *mut c_void);
}

/// 固定错误，不携带 HRESULT、OS 错误、路径、配置正文或环境值。
/// caller 应将所有 Err 记为来源 Unknown，不能记 KnownAbsent 或猜默认位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SystemConfigError {
    KnownFolderUnavailable,
    NullPath,
    PathLimit,
    InvalidPath,
}

impl SystemConfigError {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::KnownFolderUnavailable => "CCW_KNOWN_FOLDER_UNAVAILABLE",
            Self::NullPath => "CCW_NULL_PATH",
            Self::PathLimit => "CCW_PATH_LIMIT",
            Self::InvalidPath => "CCW_INVALID_PATH",
        }
    }
}

// 独占 CoTaskMem 所有权，不实现 Copy/Clone；在调用 API 前建立守卫。
struct TaskMemWide(*mut u16);

impl Drop for TaskMemWide {
    fn drop(&mut self) {
        // SAFETY: 指针仅由 SHGetKnownFolderPath 写入。官方要求成功/失败均释放；
        // CoTaskMemFree(NULL) 无副作用。绝不使用 Rust allocator/CloseHandle。
        unsafe { CoTaskMemFree(self.0.cast()) };
    }
}

/// 返回实际 Known Folder 位置下的 System config 文件名，不证明文件存在或生效。
/// 不读取 PROGRAMDATA/HOME，不提供硬编码目录回退，不加载 managed/MDM。
pub(super) fn system_config_path() -> Result<PathBuf, SystemConfigError> {
    let mut owned = TaskMemWide(ptr::null_mut());
    // SAFETY: GUID 的 C 布局/宽度如上；out 为可写的独占指针槽，初值为 NULL。
    // NULL token 使用当前调用上下文；ProgramData 是系统 FIXED Known Folder。
    let hr = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_PROGRAM_DATA,
            KF_FLAG_DONT_VERIFY,
            ptr::null_mut(),
            &mut owned.0,
        )
    };
    // 该 API 只承诺 S_OK 成功；负 HRESULT 和未承诺的正数都保守返回 Unknown。
    // 失败时绝不解引用 out，但守卫仍按官方契约释放可能返回的分配。
    if hr != S_OK { return Err(SystemConfigError::KnownFolderUnavailable); }

    // SAFETY: S_OK 保证非 NULL out（若不是则由 helper 拒绝）指向 OS 分配的
    // NUL 终止 u16 序列。逐个读取直到 NUL 或预算，owned 在整个借用期间存活。
    let len = unsafe { bounded_nul_len(owned.0)? };
    // SAFETY: helper 已找到真实 NUL；只借用已逐项验证的 len 个元素，不含 NUL。
    let wide = unsafe { std::slice::from_raw_parts(owned.0, len) };
    // 在 owned 释放前复制为独立 PathBuf；失败/返回/正常 unwind 均由 Drop 释放。
    config_path_from_wide(wide)
}

/// # Safety
/// 非 NULL 指针必须对齐，并可连续读取到首个 NUL（含 NUL），或者至少能读取
/// MAX_FOLDER_UNITS 个元素。OS 的成功字符串契约提供这一保证；预算本身不能
/// 证明任意外部指针有效。NULL 可以传入，只会返回固定错误。
unsafe fn bounded_nul_len(path: *const u16) -> Result<usize, SystemConfigError> {
    if path.is_null() { return Err(SystemConfigError::NullPath); }
    for index in 0..MAX_FOLDER_UNITS {
        // SAFETY: 调用者保证连续可读；找到 NUL 立即结束，不访问后续元素。
        if unsafe { path.add(index).read() } == 0 { return Ok(index); }
    }
    Err(SystemConfigError::PathLimit)
}

// 无 API 调用的纯转换部分；保留 Windows OsString（包括未配对 surrogate），
// 不用有损 UTF-8，也不把无法解码的真实路径打印到错误中。
fn config_path_from_wide(wide: &[u16]) -> Result<PathBuf, SystemConfigError> {
    if wide.len() >= MAX_FOLDER_UNITS { return Err(SystemConfigError::PathLimit); }
    if wide.is_empty() || wide.contains(&0) { return Err(SystemConfigError::InvalidPath); }
    let mut path = PathBuf::from(OsString::from_wide(wide));
    if !path.is_absolute() { return Err(SystemConfigError::InvalidPath); }
    // 在追加后缀前检查。from_wide 的转换分配也已由 u16 预算限制。
    if path.as_os_str().len().saturating_add(1 + CONFIG_SUFFIX.len()) > MAX_CONFIG_PATH_BYTES {
        return Err(SystemConfigError::PathLimit);
    }
    path.push(CONFIG_SUFFIX);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;

    // 纯合成数据；这些 fixtures 不调用 system_config_path 或其他 Windows API。
    #[test]
    fn relocated_and_unc_locations_keep_native_base() {
        for base in [r"D:\relocated-data", r"\\server\share\data", r"\\?\E:\数据"] {
            let wide = base.encode_utf16().collect::<Vec<_>>();
            assert_eq!(config_path_from_wide(&wide).unwrap(), PathBuf::from(base).join(CONFIG_SUFFIX));
        }
    }

    #[test]
    fn empty_embedded_nul_relative_drive_relative_and_root_relative_fail() {
        for raw in ["", "relative", r"C:relative", r"\root-relative", "C:\\bad\0tail"] {
            let wide = raw.encode_utf16().collect::<Vec<_>>();
            assert_eq!(config_path_from_wide(&wide), Err(SystemConfigError::InvalidPath));
        }
    }

    #[test]
    fn conversion_preserves_non_unicode_windows_code_units() {
        let mut wide = r"D:\data-".encode_utf16().collect::<Vec<_>>();
        wide.push(0xd800); // Windows 路径可包含未配对 surrogate。
        let expected = PathBuf::from(OsString::from_wide(&wide)).join(CONFIG_SUFFIX);
        let actual = config_path_from_wide(&wide).unwrap();
        assert_eq!(actual.as_os_str().encode_wide().collect::<Vec<_>>(),
            expected.as_os_str().encode_wide().collect::<Vec<_>>());
    }

    #[test]
    fn scan_stops_at_first_nul_and_handles_null() {
        let raw = [b'D' as u16, b':' as u16, b'\\' as u16, 0, b'x' as u16];
        // SAFETY: 合成数组在首个 NUL 之前和 NUL 位置均可读。
        assert_eq!(unsafe { bounded_nul_len(raw.as_ptr()) }, Ok(3));
        // SAFETY: helper 明确支持 NULL，不解引用。
        assert_eq!(unsafe { bounded_nul_len(ptr::null()) }, Err(SystemConfigError::NullPath));
    }

    #[test]
    fn scan_has_exact_bound_without_requiring_a_max_sized_borrow() {
        let mut raw = vec![1u16; MAX_FOLDER_UNITS];
        // SAFETY: 数组恰好可读取 MAX_FOLDER_UNITS 项；不会读取第 MAX 项。
        assert_eq!(unsafe { bounded_nul_len(raw.as_ptr()) }, Err(SystemConfigError::PathLimit));
        raw[MAX_FOLDER_UNITS - 1] = 0;
        // SAFETY: 与上一调用相同，最后一个允许的元素是 NUL。
        assert_eq!(unsafe { bounded_nul_len(raw.as_ptr()) }, Ok(MAX_FOLDER_UNITS - 1));
    }

    #[test]
    fn wide_and_final_encoded_path_budgets_include_suffix() {
        assert_eq!(config_path_from_wide(&vec![1u16; MAX_FOLDER_UNITS]), Err(SystemConfigError::PathLimit));
        let base = format!("D:\\{}", "x".repeat(MAX_CONFIG_PATH_BYTES - 3 - 1 - CONFIG_SUFFIX.len()));
        let wide = base.encode_utf16().collect::<Vec<_>>();
        let path = config_path_from_wide(&wide).unwrap();
        assert_eq!(path.as_os_str().len(), MAX_CONFIG_PATH_BYTES);
        let oversized = format!("{base}x").encode_utf16().collect::<Vec<_>>();
        assert_eq!(config_path_from_wide(&oversized), Err(SystemConfigError::PathLimit));
    }
}
