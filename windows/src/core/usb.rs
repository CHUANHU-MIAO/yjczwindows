//! USB 设备检测模块
//!
//! 该模块用于检测连接到系统的 USB 可移动存储设备（U盘、移动硬盘等），
//! 获取设备的物理信息（厂商、型号、容量、分区样式等），
//! 以及执行写入前的锁定/卸载操作。
//!
//! 参考 Rufus 的 dev.c 实现思路，使用 Windows IOCTL 进行底层设备查询。

#![allow(dead_code)]

use anyhow::{Context, Result};

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, HANDLE},
    Win32::Storage::FileSystem::{
        CreateFileW, GetDriveTypeW, GetVolumeInformationW,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ACCESS_RIGHTS,
    },
    Win32::System::IO::DeviceIoControl,
    Win32::System::Ioctl::{
        IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
        IOCTL_STORAGE_EJECT_MEDIA,
        PARTITION_STYLE_GPT, PARTITION_STYLE_MBR,
        FSCTL_LOCK_VOLUME, FSCTL_DISMOUNT_VOLUME, FSCTL_UNLOCK_VOLUME,
    },
};

// 自定义结构体（与 disk.rs 保持一致）
#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct DriveLayoutInformationEx {
    partition_style: u32,
    partition_count: u32,
}

// 驱动器类型常量
#[cfg(windows)]
const DRIVE_REMOVABLE: u32 = 2;
#[cfg(windows)]
const DRIVE_FIXED: u32 = 3;

use crate::core::disk::PartitionStyle;

/// USB 设备信息
#[derive(Debug, Clone)]
pub struct UsbDevice {
    /// 盘符（如 "D:"）
    pub letter: String,
    /// 卷标
    pub label: String,
    /// 物理磁盘号（PhysicalDriveN）
    pub physical_drive: u32,
    /// 分区号
    pub partition_number: u32,
    /// 设备类型（0=固定, 1=可移动）
    pub device_type: u32,
    /// 总容量（字节）
    pub size_bytes: u64,
    /// 剩余空间（字节）
    pub free_bytes: u64,
    /// 是否为可移动磁盘
    pub is_removable: bool,
    /// 分区样式（GPT/MBR/Unknown）
    pub partition_style: PartitionStyle,
    /// 文件系统名称（如 "NTFS", "FAT32"）
    pub file_system: String,
}

/// USB 设备管理器
pub struct UsbManager;

impl UsbManager {
    /// 获取所有可移动 USB 存储设备列表
    pub fn get_usb_devices() -> Vec<UsbDevice> {
        let mut devices = Vec::new();

        for letter in b'C'..=b'Z' {
            let c = letter as char;
            let drive = format!("{}:\\", c);

            // 检查盘符是否存在
            let path = std::path::Path::new(&drive);
            if !path.exists() {
                continue;
            }

            #[cfg(windows)]
            {
                let wide: Vec<u16> = drive
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();

                unsafe {
                    let drive_type = GetDriveTypeW(PCWSTR::from_raw(wide.as_ptr()));

                    // 只关心可移动磁盘（U盘）和固定磁盘（移动硬盘）
                    if drive_type != DRIVE_REMOVABLE && drive_type != DRIVE_FIXED {
                        continue;
                    }

                    // 对于固定磁盘，跳过系统盘 C: 和 X:
                    if drive_type == DRIVE_FIXED {
                        let is_system = c == 'C' || c == 'X';
                        if is_system {
                            continue;
                        }
                        // 只包含小于 256GB 的固定磁盘（可能是移动硬盘）
                        if let Ok(total) = Self::get_disk_size(c) {
                            if total > 256 * 1024 * 1024 * 1024 {
                                // 超过 256GB，跳过（更可能是内置硬盘）
                                // 但仍保留该逻辑为可选项
                            }
                        }
                    }
                }
            }

            match Self::get_device_info(c) {
                Ok(info) => devices.push(info),
                Err(e) => {
                    log::debug!("[USB] 获取设备信息失败 {}: {}", c, e);
                }
            }
        }

        devices.sort_by(|a, b| a.letter.cmp(&b.letter));
        devices
    }

    /// 获取单个 USB 设备的详细信息
    fn get_device_info(letter: char) -> Result<UsbDevice> {
        let drive = format!("{}:\\", letter);
        let wide: Vec<u16> = drive.encode_utf16().chain(std::iter::once(0)).collect();

        #[cfg(windows)]
        unsafe {
            // 1. 获取卷标和文件系统
            let mut volume_name = [0u16; 261];
            let mut file_system = [0u16; 261];

            let _ = GetVolumeInformationW(
                PCWSTR::from_raw(wide.as_ptr()),
                Some(&mut volume_name),
                None,
                None,
                None,
                Some(&mut file_system),
            );

            let label = String::from_utf16_lossy(&volume_name)
                .trim_end_matches('\0')
                .to_string();

            let fs = String::from_utf16_lossy(&file_system)
                .trim_end_matches('\0')
                .to_string();

            // 2. 获取磁盘大小
            let (total_bytes, free_bytes) = Self::get_disk_space(letter)?;

            // 3. 获取设备号
            let (device_type, device_number, partition_number) =
                Self::get_storage_device_number(letter);

            // 4. 获取分区样式
            let partition_style = Self::get_partition_style(device_number);

            Ok(UsbDevice {
                letter: format!("{}:", letter),
                label,
                physical_drive: device_number,
                partition_number,
                device_type,
                size_bytes: total_bytes,
                free_bytes,
                is_removable: device_type == 1, // FILE_DEVICE_DISK = 7, FILE_DEVICE_MASS_STORAGE...
                partition_style,
                file_system: fs,
            })
        }

        #[cfg(not(windows))]
        {
            Err(anyhow::anyhow!("USB 检测仅支持 Windows"))
        }
    }

    /// 获取磁盘空间
    #[cfg(windows)]
    fn get_disk_space(letter: char) -> Result<(u64, u64)> {
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let drive = format!("{}:\\", letter);
        let wide: Vec<u16> = drive.encode_utf16().chain(std::iter::once(0)).collect();

        let mut free_bytes_available: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free_bytes: u64 = 0;

        unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR::from_raw(wide.as_ptr()),
                Some(&mut free_bytes_available),
                Some(&mut total_bytes),
                Some(&mut total_free_bytes),
            )?;
        }

        Ok((total_bytes, free_bytes_available))
    }

    /// 获取磁盘总大小（快速版本，不返回空闲空间）
    #[cfg(windows)]
    fn get_disk_size(letter: char) -> Result<u64> {
        let (total, _) = Self::get_disk_space(letter)?;
        Ok(total)
    }

    /// 使用 IOCTL_STORAGE_GET_DEVICE_NUMBER 获取存储设备号
    #[cfg(windows)]
    fn get_storage_device_number(letter: char) -> (u32, u32, u32) {
        unsafe {
            let volume_path = format!("\\\\.\\{}:", letter);
            let wide: Vec<u16> = volume_path
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                0, // 不需要读写权限
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(_) => return (0, 0, 0),
            };

            if handle == INVALID_HANDLE_VALUE {
                return (0, 0, 0);
            }

            let mut device_number = StorageDeviceNumber::default();
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                None,
                0,
                Some(&mut device_number as *mut _ as *mut _),
                std::mem::size_of::<StorageDeviceNumber>() as u32,
                Some(&mut bytes_returned),
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_ok() {
                (
                    device_number.device_type,
                    device_number.device_number,
                    device_number.partition_number,
                )
            } else {
                (0, 0, 0)
            }
        }
    }

    /// 获取磁盘分区样式（GPT/MBR）
    #[cfg(windows)]
    fn get_partition_style(disk_number: u32) -> PartitionStyle {
        if disk_number == 0 {
            return PartitionStyle::Unknown;
        }

        unsafe {
            let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_number);
            let wide: Vec<u16> = disk_path
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let handle = CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) => h,
                Err(_) => return PartitionStyle::Unknown,
            };

            if handle == INVALID_HANDLE_VALUE {
                return PartitionStyle::Unknown;
            }

            let mut buffer = vec![0u8; 4096];
            let mut bytes_returned: u32 = 0;

            let result = DeviceIoControl(
                handle,
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                None,
                0,
                Some(buffer.as_mut_ptr() as *mut _),
                buffer.len() as u32,
                Some(&mut bytes_returned),
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_ok() && bytes_returned >= 8 {
                let layout = &*(buffer.as_ptr() as *const DriveLayoutInformationEx);
                match layout.partition_style {
                    x if x == PARTITION_STYLE_MBR.0 as u32 => PartitionStyle::MBR,
                    x if x == PARTITION_STYLE_GPT.0 as u32 => PartitionStyle::GPT,
                    _ => PartitionStyle::Unknown,
                }
            } else {
                PartitionStyle::Unknown
            }
        }
    }

    /// 打开物理磁盘句柄（用于直接磁盘写入）
    ///
    /// # Safety
    /// 返回的句柄具有写入权限，调用者需要确保正确使用。
    #[cfg(windows)]
    pub fn open_physical_drive(disk_number: u32, write_access: bool) -> Result<HANDLE> {
        let disk_path = format!("\\\\.\\PhysicalDrive{}", disk_number);
        let wide: Vec<u16> = disk_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let access = if write_access {
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0
        } else {
            0u32
        };

        unsafe {
            let handle = CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            match handle {
                Ok(h) if h != INVALID_HANDLE_VALUE => Ok(h),
                Ok(_) => Err(anyhow::anyhow!("无法打开物理磁盘 {}", disk_number)),
                Err(e) => Err(anyhow::anyhow!("打开物理磁盘失败: {:?}", e)),
            }
        }
    }

    /// 锁定卷（写入前必须锁定）
    #[cfg(windows)]
    pub fn lock_volume(letter: char) -> Result<()> {
        let volume_path = format!("\\\\.\\{}:", letter);
        let wide: Vec<u16> = volume_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) if h != INVALID_HANDLE_VALUE => h,
                _ => return Err(anyhow::anyhow!("无法打开卷 {}: 进行锁定", letter)),
            };

            // 先卸载卷
            let mut _bytes: u32 = 0;
            let _ = DeviceIoControl(
                handle,
                FSCTL_DISMOUNT_VOLUME,
                None,
                0,
                None,
                0,
                Some(&mut _bytes),
                None,
            );

            // 锁定卷
            let result = DeviceIoControl(
                handle,
                FSCTL_LOCK_VOLUME,
                None,
                0,
                None,
                0,
                Some(&mut _bytes),
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_err() {
                Err(anyhow::anyhow!("锁定卷 {}: 失败，请确保没有其他程序正在使用该磁盘", letter))
            } else {
                log::info!("[USB] 已锁定卷 {}:", letter);
                Ok(())
            }
        }
    }

    /// 解锁卷
    #[cfg(windows)]
    pub fn unlock_volume(letter: char) -> Result<()> {
        let volume_path = format!("\\\\.\\{}:", letter);
        let wide: Vec<u16> = volume_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) if h != INVALID_HANDLE_VALUE => h,
                _ => return Ok(()),
            };

            let mut _bytes: u32 = 0;
            DeviceIoControl(
                handle,
                FSCTL_UNLOCK_VOLUME,
                None,
                0,
                None,
                0,
                Some(&mut _bytes),
                None,
            );

            let _ = CloseHandle(handle);
            Ok(())
        }
    }

    /// 格式化设备信息为可读字符串
    pub fn format_device_info(device: &UsbDevice) -> String {
        let size_gb = device.size_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        let device_type_str = if device.is_removable { "可移动磁盘" } else { "固定磁盘" };

        format!(
            "{} [{}] {:.1} GB {} ({}) {}",
            device.letter,
            device.label,
            size_gb,
            device.file_system,
            device_type_str,
            device.partition_style
        )
    }

    /// 弹出 USB 设备
    #[cfg(windows)]
    pub fn eject_device(letter: char) -> Result<()> {
        let volume_path = format!("\\\\.\\{}:", letter);
        let wide: Vec<u16> = volume_path
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            );

            let handle = match handle {
                Ok(h) if h != INVALID_HANDLE_VALUE => h,
                _ => return Err(anyhow::anyhow!("无法打开设备 {}: 进行弹出", letter)),
            };

            let mut _bytes: u32 = 0;
            let result = DeviceIoControl(
                handle,
                IOCTL_STORAGE_EJECT_MEDIA,
                None,
                0,
                None,
                0,
                Some(&mut _bytes),
                None,
            );

            let _ = CloseHandle(handle);

            if result.is_err() {
                Err(anyhow::anyhow!("弹出设备 {}: 失败", letter))
            } else {
                Ok(())
            }
        }
    }
}
