//! 引导扇区写入模块
//!
//! 该模块负责写入 MBR（主引导记录）、PBR（分区引导记录），
//! 以及设置 UEFI 引导所需的 EFI 系统分区结构。
//!
//! 参考 Rufus 的实现方式，支持：
//! - BIOS/Legacy 引导：写入 Windows MBR 代码
//! - UEFI 引导：创建 EFI 系统分区和引导文件
//! - 双重引导：同时支持 BIOS + UEFI

#![allow(dead_code)]

use anyhow::{Context, Result};
use std::path::Path;

use crate::utils::cmd::create_command;
use crate::utils::path::get_bin_dir;

/// 引导类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootType {
    /// BIOS / Legacy 引导（使用 MBR）
    Bios,
    /// UEFI 引导（使用 GPT + EFI 分区）
    Uefi,
    /// BIOS + UEFI 双重引导
    Both,
}

/// 目标系统类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSystem {
    /// BIOS 或 UEFI-CSM
    BiosOrUefiCsm,
    /// 纯 UEFI
    UefiOnly,
}

/// 引导扇区管理器
pub struct BootSectorManager;

impl BootSectorManager {
    /// 写入 MBR（主引导记录）
    ///
    /// 使用系统内置的 bootsect.exe 工具写入 Windows MBR。
    /// 如果 bootsect.exe 不存在，则回退到 diskpart。
    ///
    /// # 参数
    /// - `physical_drive`: 物理磁盘号（如 2 表示 PhysicalDrive2）
    /// - `mbr_type`: MBR 类型代码（/nt52=XP, /nt60=Vista+）
    pub fn write_mbr(physical_drive: u32, mbr_type: &str) -> Result<()> {
        let bootsect_path = Self::find_bootsect();

        let drive_arg = format!("\\\\.\\PhysicalDrive{}", physical_drive);

        log::info!(
            "[BOOTSECT] 写入 MBR: PhysicalDrive{} (类型={})",
            physical_drive,
            mbr_type
        );

        let output = create_command(&bootsect_path)
            .args([mbr_type, &drive_arg, "/force", "/mbr"])
            .output()
            .context("执行 bootsect.exe 失败")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            log::error!("[BOOTSECT] MBR 写入失败: stdout={}, stderr={}", stdout, stderr);

            // 如果 bootsect 失败，尝试使用 diskpart
            Self::write_mbr_diskpart(physical_drive)?;
        } else {
            log::info!("[BOOTSECT] MBR 写入成功");
        }

        Ok(())
    }

    /// 使用 diskpart 写入 MBR（后备方案）
    fn write_mbr_diskpart(physical_drive: u32) -> Result<()> {
        // 查找 diskpart
        let diskpart_path = Self::find_diskpart();

        let script = format!(
            "select disk {}\nclean\nconvert mbr\ncreate partition primary\nselect partition 1\nactive\n",
            physical_drive
        );

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("lr_mbr_script.txt");
        std::fs::write(&script_path, &script)?;

        log::info!("[BOOTSECT] 使用 diskpart 创建 MBR 分区");

        let output = create_command(&diskpart_path)
            .args(["/s", script_path.to_str().unwrap()])
            .output()
            .context("执行 diskpart 失败")?;

        let _ = std::fs::remove_file(&script_path);

        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!("diskpart MBR 创建失败: {}", stdout);
        }

        Ok(())
    }

    /// 写入 PBR（分区引导记录）
    ///
    /// # 参数
    /// - `letter`: 目标分区盘符
    /// - `pbr_type`: PBR 类型（/nt60=NTLDR)
    pub fn write_pbr(letter: char, pbr_type: &str) -> Result<()> {
        let bootsect_path = Self::find_bootsect();
        let drive = format!("{}:", letter);

        log::info!("[BOOTSECT] 写入 PBR: {}: (类型={})", letter, pbr_type);

        let output = create_command(&bootsect_path)
            .args([pbr_type, &drive, "/force"])
            .output()
            .context("执行 bootsect.exe 失败")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("[BOOTSECT] PBR 写入可能失败: {}", stderr);
            // PBR 写入失败不一定致命，继续执行
        } else {
            log::info!("[BOOTSECT] PBR 写入成功");
        }

        Ok(())
    }

    /// 在目标分区上设置 BCD 引导
    ///
    /// 使用 bcdboot.exe 创建引导配置。
    ///
    /// # 参数
    /// - `source_partition`: Windows 源分区（如 "C:"）
    /// - `target_partition`: 引导目标分区（通常与 source 相同）
    /// - `firmware`: 固件类型（"UEFI" 或 "BIOS"）
    pub fn setup_bcd(
        source_partition: &str,
        target_partition: &str,
        firmware: &str,
    ) -> Result<()> {
        let bcdboot_path = Self::find_bcdboot();

        let source_windows = format!("{}\\Windows", source_partition);

        if !Path::new(&source_windows).exists() {
            anyhow::bail!(
                "源分区 {} 中未找到 Windows 目录，无法设置引导",
                source_partition
            );
        }

        log::info!(
            "[BOOTSECT] 设置 BCD 引导: 源={}, 目标={}, 固件={}",
            source_partition,
            target_partition,
            firmware
        );

        let firmware_arg = format!("/f {}", firmware);

        let output = create_command(&bcdboot_path)
            .args([&source_windows, "/s", target_partition, &firmware_arg, "/v"])
            .output()
            .context("执行 bcdboot.exe 失败")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            log::error!(
                "[BOOTSECT] BCD 设置失败: stdout={}, stderr={}",
                stdout,
                stderr
            );
            // BCD 设置失败不一定是致命的
        } else {
            log::info!("[BOOTSECT] BCD 引导设置成功");
        }

        Ok(())
    }

    /// 复制 EFI 引导文件（用于 UEFI 启动）
    ///
    /// 确保目标 FAT32 分区根目录下存在 EFI\BOOT\BOOTx64.EFI
    pub fn copy_efi_files(source_windows_dir: &str, efi_partition: &str) -> Result<()> {
        let efi_boot_dir = format!("{}\\EFI\\BOOT", efi_partition);
        std::fs::create_dir_all(&efi_boot_dir)
            .context("创建 EFI/BOOT 目录失败")?;

        // 从 Windows 源目录复制 EFI 引导文件
        let src_bootmgfw = Path::new(source_windows_dir)
            .join("Boot")
            .join("EFI")
            .join("bootmgfw.efi");

        let dst_bootx64 = Path::new(&efi_boot_dir).join("BOOTx64.EFI");

        if src_bootmgfw.exists() {
            log::info!(
                "[BOOTSECT] 复制 EFI 引导文件: {} -> {}",
                src_bootmgfw.display(),
                dst_bootx64.display()
            );
            std::fs::copy(&src_bootmgfw, &dst_bootx64)
                .context("复制 EFI 引导文件失败")?;
        } else {
            // 尝试使用 bcdboot 来生成 EFI 文件
            log::info!("[BOOTSECT] 源 EFI 文件不存在，使用 bcdboot 生成");
            let bcdboot_path = Self::find_bcdboot();
            let source = format!("{}\\Windows", source_windows_dir);

            let _ = create_command(&bcdboot_path)
                .args([&source, "/s", efi_partition, "/f", "UEFI"])
                .output();
        }

        Ok(())
    }

    /// 清理磁盘分区表（相当于 diskpart clean）
    pub fn clean_disk(physical_drive: u32) -> Result<()> {
        let diskpart_path = Self::find_diskpart();

        let script = format!("select disk {}\nclean\n", physical_drive);

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("lr_clean_script.txt");
        std::fs::write(&script_path, &script)?;

        log::info!("[BOOTSECT] 清理磁盘 PhysicalDrive{}", physical_drive);

        let output = create_command(&diskpart_path)
            .args(["/s", script_path.to_str().unwrap()])
            .output()
            .context("执行 diskpart clean 失败")?;

        let _ = std::fs::remove_file(&script_path);

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.to_lowercase().contains("error") || stdout.to_lowercase().contains("失败") {
            anyhow::bail!("diskpart clean 失败: {}", stdout);
        }

        log::info!("[BOOTSECT] 磁盘清理完成");
        Ok(())
    }

    /// 创建分区（MBR 模式）
    pub fn create_mbr_partition(physical_drive: u32, active: bool) -> Result<()> {
        let diskpart_path = Self::find_diskpart();

        let active_line = if active { "active\n" } else { "" };

        let script = format!(
            "select disk {}\nconvert mbr\ncreate partition primary\n{}select partition 1\n",
            physical_drive, active_line
        );

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("lr_mbr_part_script.txt");
        std::fs::write(&script_path, &script)?;

        log::info!(
            "[BOOTSECT] 创建 MBR 分区: PhysicalDrive{} (active={})",
            physical_drive,
            active
        );

        let output = create_command(&diskpart_path)
            .args(["/s", script_path.to_str().unwrap()])
            .output()
            .context("执行 diskpart 分区创建失败")?;

        let _ = std::fs::remove_file(&script_path);

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.to_lowercase().contains("error") || stdout.to_lowercase().contains("失败") {
            anyhow::bail!("diskpart 分区创建失败: {}", stdout);
        }

        Ok(())
    }

    /// 创建分区（GPT 模式，含 EFI 系统分区）
    pub fn create_gpt_partitions(
        physical_drive: u32,
        efi_size_mb: u32,
        data_size_mb: u32,
    ) -> Result<()> {
        let diskpart_path = Self::find_diskpart();

        let script = if data_size_mb > 0 {
            format!(
                "select disk {}\n\
                 convert gpt\n\
                 create partition efi size={}\n\
                 format quick fs=fat32 label=\"EFI\"\n\
                 assign letter=\"S\"\n\
                 create partition primary size={}\n\
                 format quick fs=ntfs label=\"DATA\"\n\
                 select partition 2\n",
                physical_drive, efi_size_mb, data_size_mb
            )
        } else {
            format!(
                "select disk {}\n\
                 convert gpt\n\
                 create partition efi size={}\n\
                 format quick fs=fat32 label=\"EFI\"\n\
                 assign letter=\"S\"\n\
                 create partition primary\n\
                 format quick fs=ntfs label=\"DATA\"\n\
                 select partition 2\n",
                physical_drive, efi_size_mb
            )
        };

        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("lr_gpt_part_script.txt");
        std::fs::write(&script_path, &script)?;

        log::info!(
            "[BOOTSECT] 创建 GPT 分区: PhysicalDrive{} (EFI={}MB)",
            physical_drive,
            efi_size_mb
        );

        let output = create_command(&diskpart_path)
            .args(["/s", script_path.to_str().unwrap()])
            .output()
            .context("执行 diskpart GPT 分区创建失败")?;

        let _ = std::fs::remove_file(&script_path);

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.to_lowercase().contains("error") || stdout.to_lowercase().contains("失败") {
            anyhow::bail!("diskpart GPT 分区创建失败: {}", stdout);
        }

        Ok(())
    }

    // ======================== 工具函数 ========================

    /// 查找 bootsect.exe 路径
    fn find_bootsect() -> String {
        let builtin = get_bin_dir().join("bootsect.exe");
        if builtin.exists() {
            builtin.to_string_lossy().to_string()
        } else {
            "bootsect.exe".to_string() // 使用系统路径
        }
    }

    /// 查找 bcdboot.exe 路径
    fn find_bcdboot() -> String {
        let builtin = get_bin_dir().join("bcdboot.exe");
        if builtin.exists() {
            builtin.to_string_lossy().to_string()
        } else {
            "bcdboot.exe".to_string()
        }
    }

    /// 查找 diskpart.exe 路径
    fn find_diskpart() -> String {
        let builtin = get_bin_dir().join("diskpart").join("diskpart.exe");
        if builtin.exists() {
            builtin.to_string_lossy().to_string()
        } else {
            "diskpart.exe".to_string()
        }
    }
}
