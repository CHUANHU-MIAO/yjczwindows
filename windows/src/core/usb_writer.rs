//! USB 启动盘写入引擎
//!
//! 该模块编排完整的"ISO → 启动U盘"制作流程：
//! 1. 检测并选择 USB 设备
//! 2. 锁定并卸载目标卷
//! 3. 清理并重新分区（MBR 或 GPT）
//! 4. 格式化（FAT32/NTFS/exFAT）
//! 5. 挂载 ISO 并复制文件
//! 6. 写入引导扇区
//!
//! 参考 Rufus 的整体工作流程设计。

#![allow(dead_code)]

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::mpsc;

use crate::core::bootsect::{BootSectorManager, TargetSystem};
use crate::core::fat32::{FsType, VolumeFormatter};
use crate::core::iso::IsoMounter;
use crate::core::usb::{UsbDevice, UsbManager};

/// USB 写入配置
#[derive(Debug, Clone)]
pub struct UsbWriteConfig {
    /// USB 设备信息
    pub device: UsbDevice,
    /// ISO 镜像路径
    pub iso_path: String,
    /// 分区方案
    pub partition_scheme: PartitionScheme,
    /// 目标系统类型
    pub target_system: TargetSystem,
    /// 文件系统类型
    pub file_system: FsType,
    /// 卷标
    pub volume_label: String,
    /// 是否快速格式化
    pub quick_format: bool,
    /// EFI 分区大小（MB，仅 GPT 方案）
    pub efi_size_mb: u32,
}

/// 分区方案
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionScheme {
    /// MBR 分区表（兼容 BIOS/Legacy）
    Mbr,
    /// GPT 分区表（UEFI）
    Gpt,
}

impl std::fmt::Display for PartitionScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionScheme::Mbr => write!(f, "MBR"),
            PartitionScheme::Gpt => write!(f, "GPT"),
        }
    }
}

/// 写入进度
#[derive(Debug, Clone)]
pub struct WriteProgress {
    /// 当前步骤描述
    pub step: String,
    /// 当前步骤进度 (0-100)
    pub step_progress: u8,
    /// 总体进度 (0-100)
    pub total_progress: u8,
    /// 是否完成
    pub finished: bool,
    /// 错误消息（如果有）
    pub error: Option<String>,
}

/// USB 启动盘写入器
pub struct UsbWriter {
    config: UsbWriteConfig,
    progress_tx: Option<mpsc::Sender<WriteProgress>>,
}

impl UsbWriter {
    /// 创建 USB 写入器
    pub fn new(config: UsbWriteConfig) -> Self {
        Self {
            config,
            progress_tx: None,
        }
    }

    /// 设置进度回调
    pub fn with_progress(mut self, tx: mpsc::Sender<WriteProgress>) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// 发送进度更新
    fn send_progress(&self, step: &str, step_pct: u8, total_pct: u8) {
        if let Some(ref tx) = self.progress_tx {
            let _ = tx.send(WriteProgress {
                step: step.to_string(),
                step_progress: step_pct,
                total_progress: total_pct,
                finished: false,
                error: None,
            });
        }
    }

    /// 发送完成信号
    fn send_finished(&self) {
        if let Some(ref tx) = self.progress_tx {
            let _ = tx.send(WriteProgress {
                step: "完成".to_string(),
                step_progress: 100,
                total_progress: 100,
                finished: true,
                error: None,
            });
        }
    }

    /// 发送错误信号
    fn send_error(&self, error: &str) {
        if let Some(ref tx) = self.progress_tx {
            let _ = tx.send(WriteProgress {
                step: "错误".to_string(),
                step_progress: 0,
                total_progress: 0,
                finished: true,
                error: Some(error.to_string()),
            });
        }
    }

    /// 执行完整的 ISO→USB 写入流程
    pub fn write(&self) -> Result<()> {
        log::info!(
            "[USBWRITE] ========== 开始制作启动盘 =========="
        );
        log::info!(
            "[USBWRITE] 设备: {} ({})",
            self.config.device.letter,
            if self.config.device.is_removable {
                "可移动磁盘"
            } else {
                "固定磁盘"
            }
        );
        log::info!("[USBWRITE] ISO: {}", self.config.iso_path);
        log::info!(
            "[USBWRITE] 方案: {} / {}",
            self.config.partition_scheme,
            self.config.file_system.as_fs_str()
        );

        let letter = self.config.device.letter
            .chars()
            .next()
            .unwrap_or('D');

        // ========================================================
        // 步骤 1: 锁定并卸载目标卷 (0-5%)
        // ========================================================
        self.send_progress("正在准备目标设备...", 0, 0);

        log::info!("[USBWRITE] 步骤 1/7: 锁定并卸载卷 {}:", letter);
        UsbManager::lock_volume(letter)?;
        self.send_progress("目标设备已准备", 100, 5);

        // ========================================================
        // 步骤 2: 清理磁盘并重新分区 (5-20%)
        // ========================================================
        self.send_progress("正在清理磁盘分区...", 0, 10);

        let physical_drive = self.config.device.physical_drive;

        // 清理磁盘
        log::info!(
            "[USBWRITE] 步骤 2/7: 清理磁盘 PhysicalDrive{}",
            physical_drive
        );
        BootSectorManager::clean_disk(physical_drive)?;
        self.send_progress("磁盘已清理", 50, 15);

        // 重新分区
        match self.config.partition_scheme {
            PartitionScheme::Mbr => {
                log::info!("[USBWRITE] 创建 MBR 分区表");
                BootSectorManager::create_mbr_partition(physical_drive, true)?;
            }
            PartitionScheme::Gpt => {
                log::info!("[USBWRITE] 创建 GPT 分区表（含 EFI 分区）");
                let efi_size = self.config.efi_size_mb;
                // 数据分区使用剩余空间（设为 0 即全部剩余）
                BootSectorManager::create_gpt_partitions(physical_drive, efi_size, 0)?;
            }
        }
        self.send_progress("分区表已创建", 100, 20);

        // ========================================================
        // 步骤 3: 格式化分区 (20-40%)
        // ========================================================
        self.send_progress("正在格式化分区...", 0, 25);

        log::info!(
            "[USBWRITE] 步骤 3/7: 格式化 {}: -> {}",
            letter,
            self.config.file_system.as_fs_str()
        );

        // 使用 fmifs.dll 格式化
        let formatter = VolumeFormatter::new()?;
        match formatter.format(
            letter,
            self.config.file_system,
            &self.config.volume_label,
            self.config.quick_format,
        ) {
            Ok(()) => {
                log::info!("[USBWRITE] fmifs.dll 格式化成功");
            }
            Err(e) => {
                log::warn!(
                    "[USBWRITE] fmifs.dll 格式化失败: {}，尝试 format.com 后备方案",
                    e
                );
                // 后备方案：使用 format.com
                VolumeFormatter::format_fallback(
                    letter,
                    self.config.file_system,
                    &self.config.volume_label,
                    self.config.quick_format,
                )?;
            }
        }

        self.send_progress("格式化完成", 100, 40);

        // ========================================================
        // 步骤 4: 挂载 ISO 镜像 (40-50%)
        // ========================================================
        self.send_progress("正在挂载 ISO 镜像...", 0, 45);

        log::info!("[USBWRITE] 步骤 4/7: 挂载 ISO: {}", self.config.iso_path);

        let iso_drive = IsoMounter::mount_iso(&self.config.iso_path)?;

        log::info!("[USBWRITE] ISO 挂载成功，盘符: {}", iso_drive);

        self.send_progress("ISO 已挂载", 100, 50);

        // ========================================================
        // 步骤 5: 复制文件 (50-85%)
        // ========================================================
        self.send_progress("正在复制文件到 U盘...", 0, 55);

        log::info!(
            "[USBWRITE] 步骤 5/7: 复制文件 {} -> {}:",
            iso_drive,
            letter
        );

        match Self::copy_files_with_progress(
            &iso_drive,
            &format!("{}:", letter),
            &self.progress_tx,
            55,
            80,
        ) {
            Ok(_) => {
                log::info!("[USBWRITE] 文件复制完成");
            }
            Err(e) => {
                log::warn!("[USBWRITE] robocopy 复制失败: {}，尝试 Rust 原生复制", e);
                Self::copy_files_rust(&iso_drive, &format!("{}:", letter))?;
            }
        }

        self.send_progress("文件复制完成", 100, 80);

        // ========================================================
        // 步骤 6: 写入引导扇区 (80-95%)
        // ========================================================
        self.send_progress("正在写入引导记录...", 0, 85);

        log::info!("[USBWRITE] 步骤 6/7: 写入引导扇区");

        match self.config.partition_scheme {
            PartitionScheme::Mbr => {
                // MBR: 写入 MBR + PBR
                BootSectorManager::write_mbr(physical_drive, "/nt60")?;
                BootSectorManager::write_pbr(letter, "/nt60")?;
            }
            PartitionScheme::Gpt => {
                // GPT: UEFI 引导已通过分区表设置，还需要复制 EFI 文件
                if Path::new(&format!("S:\\EFI")).exists()
                    || Path::new(&format!("{}:\\EFI\\BOOT", letter)).exists()
                {
                    log::info!("[USBWRITE] EFI 分区已存在，跳过复制");
                } else {
                    // 从 ISO 的 EFI 目录复制到 U盘的 EFI 目录
                    let efi_src = format!("{}\\EFI", iso_drive.trim_end_matches(':'));
                    let efi_dst = format!("{}\\EFI", letter);
                    if Path::new(&efi_src).exists() {
                        Self::copy_dir_recursive(&efi_src, &efi_dst)?;
                    }
                }
            }
        }

        self.send_progress("引导记录已写入", 100, 90);

        // ========================================================
        // 步骤 7: 卸载 ISO, 清理 (95-100%)
        // ========================================================
        self.send_progress("正在清理...", 50, 95);

        log::info!("[USBWRITE] 步骤 7/7: 卸载 ISO 和清理");

        // 卸载 ISO
        let _ = IsoMounter::unmount();

        // 解锁卷
        let _ = UsbManager::unlock_volume(letter);

        self.send_progress("启动盘制作完成！", 100, 100);
        self.send_finished();

        log::info!(
            "[USBWRITE] ========== 启动盘制作完成 =========="
        );
        Ok(())
    }

    /// 使用 robocopy 复制文件（带进度）
    fn copy_files_with_progress(
        source: &str,
        dest: &str,
        progress_tx: &Option<mpsc::Sender<WriteProgress>>,
        progress_start: u8,
        progress_end: u8,
    ) -> Result<()> {
        // robocopy 速度快，是 Windows 上最可靠的文件复制工具
        let source_path = format!("{}\\", source);
        let dest_path = format!("{}\\", dest);

        log::info!(
            "[USBWRITE] robocopy: {} -> {}",
            source_path,
            dest_path
        );

        let mut cmd = std::process::Command::new("robocopy");

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let child = cmd
            .args([
                &source_path,
                &dest_path,
                "/E",       // 复制子目录，包括空目录
                "/Z",       // 可重启模式
                "/NP",      // 不显示进度百分比
                "/NDL",     // 不显示目录列表
                "/NJH",     // 不显示作业头
                "/NJS",     // 不显示作业摘要
                "/R:2",     // 重试 2 次
                "/W:2",     // 重试等待 2 秒
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("启动 robocopy 失败")?;

        let output = child.wait_with_output()
            .context("等待 robocopy 完成失败")?;

        // robocopy 成功返回 0 或 1（1 = 文件已复制但可能有额外文件）
        if output.status.code().unwrap_or(1) > 7 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("robocopy 失败 (退出码 {}): {}",
                output.status.code().unwrap_or(-1),
                stderr
            );
        }

        Ok(())
    }

    /// Rust 原生文件复制（后备方案）
    fn copy_files_rust(source: &str, dest: &str) -> Result<()> {
        let source_path = format!("{}\\", source);
        let dest_path = format!("{}\\", dest);

        log::info!(
            "[USBWRITE] Rust 原生复制: {} -> {}",
            source_path,
            dest_path
        );

        // 使用 walkdir 遍历源目录
        for entry in walkdir::WalkDir::new(&source_path)
            .follow_links(false)
        {
            let entry = entry.context("遍历目录失败")?;
            let path = entry.path();

            // 计算相对路径
            let relative = path
                .strip_prefix(&source_path)
                .context("计算相对路径失败")?;

            let target = Path::new(&dest_path).join(relative);

            if path.is_dir() {
                std::fs::create_dir_all(&target)
                    .context(format!("创建目录失败: {}", target.display()))?;
            } else if path.is_file() {
                // 对于大文件，使用优化的复制方式
                if let Ok(metadata) = path.metadata() {
                    if metadata.len() > 10 * 1024 * 1024 {
                        // 大于 10MB 的文件使用 buffer
                        Self::copy_large_file(path, &target)?;
                    } else {
                        std::fs::copy(path, &target)
                            .context(format!("复制文件失败: {}", path.display()))?;
                    }
                } else {
                    std::fs::copy(path, &target)
                        .context(format!("复制文件失败: {}", path.display()))?;
                }
            }
        }

        Ok(())
    }

    /// 复制大文件（使用缓冲区优化）
    fn copy_large_file(source: &Path, dest: &Path) -> Result<()> {
        use std::io::{BufReader, BufWriter, Read, Write};

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let src_file = std::fs::File::open(source)?;
        let dst_file = std::fs::File::create(dest)?;

        let mut reader = BufReader::with_capacity(1024 * 1024, src_file); // 1MB buffer
        let mut writer = BufWriter::with_capacity(1024 * 1024, dst_file);

        let mut buffer = vec![0u8; 1024 * 1024]; // 1MB chunk
        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            writer.write_all(&buffer[..bytes_read])?;
        }
        writer.flush()?;

        Ok(())
    }

    /// 递归复制目录
    fn copy_dir_recursive(source: &str, dest: &str) -> Result<()> {
        let src = Path::new(source);
        let dst = Path::new(dest);

        if !src.exists() {
            return Ok(());
        }

        std::fs::create_dir_all(dst)?;

        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let target = dst.join(entry.file_name());

            if path.is_dir() {
                Self::copy_dir_recursive(
                    &path.to_string_lossy(),
                    &target.to_string_lossy(),
                )?;
            } else {
                std::fs::copy(&path, &target)?;
            }
        }

        Ok(())
    }
}

/// 快速制作启动盘的便捷函数
///
/// 使用默认配置快速制作一个 Windows 启动 U 盘。
///
/// # 参数
/// - `iso_path`: Windows ISO 镜像路径
/// - `usb_letter`: U盘盘符（如 'D'）
/// - `progress_tx`: 进度回调通道
pub fn create_bootable_usb(
    iso_path: &str,
    usb_letter: char,
    progress_tx: Option<mpsc::Sender<WriteProgress>>,
) -> Result<()> {
    // 检测 USB 设备
    let devices = UsbManager::get_usb_devices();
    let device = devices
        .iter()
        .find(|d| d.letter.starts_with(usb_letter))
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("未找到 USB 设备: {}:", usb_letter)
        })?;

    // 检测 ISO 大小以决定文件系统
    let iso_size = std::fs::metadata(iso_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // 大于 4GB 的 ISO 需要使用 NTFS 或 exFAT
    let fs_type = if iso_size > 4 * 1024 * 1024 * 1024 {
        FsType::NTFS // FAT32 单文件最大 4GB
    } else {
        FsType::FAT32 // 小文件用 FAT32 兼容性最好
    };

    let config = UsbWriteConfig {
        device,
        iso_path: iso_path.to_string(),
        partition_scheme: PartitionScheme::Mbr, // 默认 MBR（兼容性最好）
        target_system: TargetSystem::BiosOrUefiCsm,
        file_system: fs_type,
        volume_label: "WINPE".to_string(),
        quick_format: true,
        efi_size_mb: 100, // EFI 分区 100MB
    };

    let writer = if let Some(tx) = progress_tx {
        UsbWriter::new(config).with_progress(tx)
    } else {
        UsbWriter::new(config)
    };

    writer.write()
}
