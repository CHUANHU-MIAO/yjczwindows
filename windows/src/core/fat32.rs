//! FAT32 文件系统格式化模块
//!
//! 该模块使用 Windows 内置的 fmifs.dll 库进行文件系统格式化，
//! 支持 FAT32、NTFS、exFAT 等多种格式。
//! 相比调用 format.com 命令行工具，直接调用 DLL 具有更好的错误处理能力。
//!
//! 参考 Rufus 的 format.c 实现思路。

#![allow(dead_code)]

use anyhow::{Context, Result};
use std::path::Path;

#[cfg(windows)]
use libloading::Library;

/// 文件系统类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    FAT,
    FAT32,
    NTFS,
    exFAT,
    UDF,
    ReFS,
}

impl FsType {
    /// 获取文件系统名称字符串（用于 fmifs）
    pub fn as_fs_str(&self) -> &'static str {
        match self {
            FsType::FAT => "FAT",
            FsType::FAT32 => "FAT32",
            FsType::NTFS => "NTFS",
            FsType::exFAT => "exFAT",
            FsType::UDF => "UDF",
            FsType::ReFS => "ReFS",
        }
    }

    /// 获取默认簇大小
    pub fn default_cluster_size(&self, disk_size_mb: u64) -> u32 {
        match self {
            FsType::FAT32 => {
                // FAT32 默认簇大小参照 Microsoft 规范
                if disk_size_mb <= 256 { 512 }          // 512 bytes
                else if disk_size_mb <= 8192 { 4096 }    // 4 KB
                else if disk_size_mb <= 16384 { 8192 }   // 8 KB
                else if disk_size_mb <= 32768 { 16384 }   // 16 KB
                else { 32768 }                             // 32 KB (FAT32 最大)
            }
            FsType::exFAT => {
                if disk_size_mb <= 256 { 4096 }
                else if disk_size_mb <= 32768 { 32768 }
                else if disk_size_mb <= 262144 { 131072 }
                else { 1048576 }  // 1 MB for large volumes
            }
            FsType::NTFS => 4096,    // NTFS 默认 4K 簇
            _ => 4096,               // 其他格式默认 4K
        }
    }

    /// 获取最大卷标长度
    pub fn max_label_length(&self) -> usize {
        match self {
            FsType::FAT | FsType::FAT32 => 11,
            FsType::NTFS => 32,
            FsType::exFAT => 11,
            _ => 32,
        }
    }
}

/// FAT32/NTFS/exFAT 格式化器
pub struct VolumeFormatter {
    #[cfg(windows)]
    _lib: Library,
    #[cfg(windows)]
    format_ex: FormatExFn,
}

#[cfg(windows)]
type FormatExFn = unsafe extern "system" fn(
    DriveRoot: *const u16,    // e.g. "D:\"
    MediaFlag: u32,           // 0 = fixed disk, 1 = removable
    Format: *const u16,       // "FAT", "FAT32", "NTFS", "exFAT"
    Label: *const u16,        // volume label
    QuickFormat: i32,         // 1 = quick format, 0 = full format
    ClusterSize: u32,         // 0 = default
    Callback: *const std::ffi::c_void, // FMIFS callback
) -> u32;

impl VolumeFormatter {
    /// 创建格式化器实例，加载 fmifs.dll
    #[cfg(windows)]
    pub fn new() -> Result<Self> {
        // fmifs.dll 位于 System32 目录
        let lib = unsafe {
            Library::new("fmifs.dll")
                .context("无法加载 fmifs.dll，请确保系统文件完整")?
        };

        let format_ex: FormatExFn = unsafe {
            *lib.get(b"FormatEx")
                .context("fmifs.dll 中未找到 FormatEx 函数")?
        };

        Ok(Self {
            format_ex,
            _lib: lib,
        })
    }

    #[cfg(not(windows))]
    pub fn new() -> Result<Self> {
        Err(anyhow::anyhow!("格式化仅支持 Windows 系统"))
    }

    /// 格式化指定的盘符
    ///
    /// # 参数
    /// - `letter`: 盘符（如 'D'）
    /// - `fs_type`: 目标文件系统类型
    /// - `label`: 卷标（可选，空字符串表示无卷标）
    /// - `quick_format`: 是否快速格式化
    ///
    /// # 示例
    /// ```ignore
    /// let formatter = VolumeFormatter::new()?;
    /// formatter.format('D', FsType::FAT32, Some("WINPE"), true)?;
    /// ```
    #[cfg(windows)]
    pub fn format(
        &self,
        letter: char,
        fs_type: FsType,
        label: &str,
        quick_format: bool,
    ) -> Result<()> {
        let root = format!("{}:\\\0", letter);
        let wide_root: Vec<u16> = root.encode_utf16().collect();

        let fs_str = format!("{}\0", fs_type.as_fs_str());
        let wide_fs: Vec<u16> = fs_str.encode_utf16().collect();

        let label_str = if label.is_empty() {
            String::from("\0")
        } else {
            // 截断到最大长度
            let max_len = fs_type.max_label_length();
            let truncated: String = label.chars().take(max_len).collect();
            format!("{}\0", truncated)
        };
        let wide_label: Vec<u16> = label_str.encode_utf16().collect();

        let media_flag: u32 = 1; // removable - 即使不是可移动磁盘也用这个标志

        let quick: i32 = if quick_format { 1 } else { 0 };

        log::info!(
            "[FAT32] 开始格式化 {}:  -> {} (快速={})",
            letter,
            fs_type.as_fs_str(),
            quick_format
        );

        let result = unsafe {
            (self.format_ex)(
                wide_root.as_ptr(),
                media_flag,
                wide_fs.as_ptr(),
                wide_label.as_ptr(),
                quick,
                0, // 使用默认簇大小
                std::ptr::null(), // 暂不使用回调
            )
        };

        if result == 0 {
            log::info!("[FAT32] 格式化 {}: 完成", letter);
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "格式化 {}: 失败，错误代码: {}",
                letter,
                result
            ))
        }
    }

    /// 格式化指定盘符，使用自定义簇大小
    #[cfg(windows)]
    pub fn format_with_cluster_size(
        &self,
        letter: char,
        fs_type: FsType,
        label: &str,
        quick_format: bool,
        cluster_size: u32,
    ) -> Result<()> {
        let root = format!("{}:\\\0", letter);
        let wide_root: Vec<u16> = root.encode_utf16().collect();

        let fs_str = format!("{}\0", fs_type.as_fs_str());
        let wide_fs: Vec<u16> = fs_str.encode_utf16().collect();

        let label_str = if label.is_empty() {
            String::from("\0")
        } else {
            let max_len = fs_type.max_label_length();
            let truncated: String = label.chars().take(max_len).collect();
            format!("{}\0", truncated)
        };
        let wide_label: Vec<u16> = label_str.encode_utf16().collect();

        let media_flag: u32 = 1;
        let quick: i32 = if quick_format { 1 } else { 0 };

        log::info!(
            "[FAT32] 开始格式化 {}: -> {} 簇大小={} (快速={})",
            letter,
            fs_type.as_fs_str(),
            cluster_size,
            quick_format
        );

        let result = unsafe {
            (self.format_ex)(
                wide_root.as_ptr(),
                media_flag,
                wide_fs.as_ptr(),
                wide_label.as_ptr(),
                quick,
                cluster_size,
                std::ptr::null(),
            )
        };

        if result == 0 {
            log::info!("[FAT32] 格式化 {}: 完成", letter);
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "格式化 {}: 失败，错误代码: {}",
                letter,
                result
            ))
        }
    }

    /// 使用系统 format.com 命令作为后备方案
    ///
    /// 当 fmifs.dll 调用失败时使用。
    pub fn format_fallback(
        letter: char,
        fs_type: FsType,
        label: &str,
        quick_format: bool,
    ) -> Result<()> {
        let partition = format!("{}:", letter);
        let fs = match fs_type {
            FsType::FAT => "/FS:FAT",
            FsType::FAT32 => "/FS:FAT32",
            FsType::NTFS => "/FS:NTFS",
            FsType::exFAT => "/FS:exFAT",
            FsType::UDF => "/FS:UDF",
            FsType::ReFS => "/FS:ReFS",
        };

        let mut args: Vec<String> = vec![partition.clone(), fs.to_string()];

        if quick_format {
            args.push("/Q".to_string());
        }

        args.push("/Y".to_string()); // 自动确认

        if !label.is_empty() {
            args.push(format!("/V:{}", label));
        }

        log::info!(
            "[FAT32] 使用 format.com 后备方案: {} {}",
            partition,
            args.join(" ")
        );

        let output = std::process::Command::new("format.com")
            .args(&args)
            .output()
            .context("执行 format.com 失败")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(anyhow::anyhow!("format.com 格式化失败: {}", stderr))
        } else {
            Ok(())
        }
    }
}
