//! 制作启动盘 UI 面板
//!
//! 在 LetRecovery 中集成 USB 启动盘制作功能，
//! 支持从 ISO 镜像创建可引导的 Windows 安装 U 盘。

use egui;
use std::sync::mpsc;

use crate::app::App;
use crate::core::bootsect::TargetSystem;
use crate::core::fat32::FsType;
use crate::core::usb::{UsbDevice, UsbManager};
use crate::core::usb_writer::{
    PartitionScheme, UsbWriteConfig, UsbWriter, WriteProgress,
};

/// USB 启动盘制作 UI 状态
pub struct UsbBootState {
    /// 可用的 USB 设备列表
    pub usb_devices: Vec<UsbDevice>,
    /// 正在刷新设备列表
    pub refreshing_devices: bool,
    /// 选中的 USB 设备索引
    pub selected_device: Option<usize>,
    /// ISO 镜像路径
    pub iso_path: String,
    /// 分区方案
    pub partition_scheme: PartitionScheme,
    /// 分区方案索引（用于下拉框）
    pub partition_scheme_idx: usize,
    /// 文件系统类型索引
    pub file_system_idx: usize,
    /// 卷标
    pub volume_label: String,
    /// 是否快速格式化
    pub quick_format: bool,
    /// 是否正在写入
    pub is_writing: bool,
    /// 写入进度
    pub write_progress: Option<WriteProgress>,
    /// 状态消息
    pub status_message: String,
    /// 进度接收器
    pub progress_rx: Option<mpsc::Receiver<WriteProgress>>,
}

impl Default for UsbBootState {
    fn default() -> Self {
        Self {
            usb_devices: Vec::new(),
            refreshing_devices: false,
            selected_device: None,
            iso_path: String::new(),
            partition_scheme: PartitionScheme::Mbr,
            partition_scheme_idx: 0,
            file_system_idx: 0, // FAT32
            volume_label: "WINPE".to_string(),
            quick_format: true,
            is_writing: false,
            write_progress: None,
            status_message: String::new(),
            progress_rx: None,
        }
    }
}

const PARTITION_SCHEMES: &[(&str, PartitionScheme)] = &[
    ("MBR (兼容 BIOS + UEFI-CSM)", PartitionScheme::Mbr),
    ("GPT (纯 UEFI)", PartitionScheme::Gpt),
];

const FILE_SYSTEMS: &[(&str, FsType)] = &[
    ("FAT32 (推荐，最大兼容性)", FsType::FAT32),
    ("NTFS (支持大于4GB文件)", FsType::NTFS),
    ("exFAT (现代格式)", FsType::exFAT),
];

impl App {
    /// 显示 USB 启动盘制作面板
    pub fn show_usb_boot(&mut self, ui: &mut egui::Ui) {
        // 延迟初始化
        if self.usb_boot_state.usb_devices.is_empty() && !self.usb_boot_state.refreshing_devices {
            self.refresh_usb_devices();
        }

        ui.heading("📀 制作启动U盘");
        ui.separator();
        ui.add_space(5.0);

        // ===== 第一步：选择 USB 设备 =====
        ui.label(egui::RichText::new("1. 选择目标 USB 设备").strong());
        ui.add_space(5.0);

        ui.horizontal(|ui| {
            if ui.button("🔄 刷新设备列表").clicked() {
                self.refresh_usb_devices();
            }

            if self.usb_boot_state.refreshing_devices {
                ui.spinner();
            }
        });

        ui.add_space(5.0);

        if self.usb_boot_state.usb_devices.is_empty() {
            if self.usb_boot_state.refreshing_devices {
                ui.label("正在扫描 USB 设备...");
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 165, 0),
                    "⚠ 未检测到 USB 存储设备，请插入 U 盘后点击刷新",
                );
            }
        } else {
            egui::ScrollArea::vertical()
                .max_height(100.0)
                .show(ui, |ui| {
                    for (i, device) in self.usb_boot_state.usb_devices.iter().enumerate() {
                        let is_selected = self.usb_boot_state.selected_device == Some(i);
                        let label = crate::core::usb::UsbManager::format_device_info(device);

                        if ui
                            .selectable_label(is_selected, &label)
                            .clicked()
                        {
                            self.usb_boot_state.selected_device = Some(i);
                        }
                    }
                });
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(5.0);

        // ===== 第二步：选择 ISO 镜像 =====
        ui.label(egui::RichText::new("2. 选择 ISO 镜像文件").strong());
        ui.add_space(5.0);

        ui.horizontal(|ui| {
            let path_edit = egui::TextEdit::singleline(
                &mut self.usb_boot_state.iso_path,
            )
            .hint_text("请选择 Windows ISO 镜像文件...")
            .min_size(egui::vec2(350.0, 20.0));

            ui.add(path_edit);

            if ui.button("📁 浏览").clicked() {
                self.browse_iso_for_usb();
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(5.0);

        // ===== 第三步：配置选项 =====
        ui.label(egui::RichText::new("3. 制作选项").strong());
        ui.add_space(5.0);

        egui::Grid::new("usb_boot_options")
            .num_columns(2)
            .spacing([20.0, 5.0])
            .show(ui, |ui| {
                // 分区方案
                ui.label("分区方案:");
                egui::ComboBox::from_id_salt("usb_partition_scheme")
                    .selected_text(PARTITION_SCHEMES[self.usb_boot_state.partition_scheme_idx].0)
                    .show_ui(ui, |ui| {
                        for (i, (label, scheme)) in PARTITION_SCHEMES.iter().enumerate() {
                            if ui.selectable_label(
                                self.usb_boot_state.partition_scheme_idx == i,
                                *label,
                            ).clicked()
                            {
                                self.usb_boot_state.partition_scheme_idx = i;
                                self.usb_boot_state.partition_scheme = *scheme;
                            }
                        }
                    });
                ui.end_row();

                // 文件系统
                ui.label("文件系统:");
                egui::ComboBox::from_id_salt("usb_file_system")
                    .selected_text(FILE_SYSTEMS[self.usb_boot_state.file_system_idx].0)
                    .show_ui(ui, |ui| {
                        for (i, (label, _)) in FILE_SYSTEMS.iter().enumerate() {
                            if ui.selectable_label(
                                self.usb_boot_state.file_system_idx == i,
                                *label,
                            ).clicked()
                            {
                                self.usb_boot_state.file_system_idx = i;
                            }
                        }
                    });
                ui.end_row();

                // 卷标
                ui.label("卷标:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.usb_boot_state.volume_label)
                        .hint_text("输入卷标...")
                        .min_size(egui::vec2(120.0, 20.0)),
                );
                ui.end_row();

                // 快速格式化
                ui.label("快速格式化:");
                ui.add(egui::Checkbox::without_text(
                    &mut self.usb_boot_state.quick_format,
                ));
                ui.end_row();
            });

        ui.add_space(10.0);

        // 警告提示
        ui.colored_label(
            egui::Color32::from_rgb(255, 80, 80),
            "⚠ 警告：制作启动盘将清除 U 盘上的所有数据！",
        );

        ui.add_space(10.0);

        // ===== 第四步：开始制作 =====
        let can_start = !self.usb_boot_state.is_writing
            && self.usb_boot_state.selected_device.is_some()
            && !self.usb_boot_state.iso_path.is_empty()
            && std::path::Path::new(&self.usb_boot_state.iso_path).exists();

        ui.horizontal(|ui| {
            let button = egui::Button::new(
                egui::RichText::new("🚀 开始制作启动盘")
                    .color(if can_start {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::GRAY
                    })
                    .strong(),
            )
            .min_size(egui::vec2(200.0, 40.0))
            .fill(if can_start {
                egui::Color32::from_rgb(0, 120, 215)
            } else {
                egui::Color32::from_rgb(100, 100, 100)
            });

            if ui.add_enabled(can_start, button).clicked() {
                self.start_usb_write();
            }

            if self.usb_boot_state.is_writing {
                ui.spinner();
                ui.label("正在制作中...");
            }
        });

        // 显示状态消息
        if !self.usb_boot_state.status_message.is_empty() {
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(5.0);

            if let Some(ref progress) = self.usb_boot_state.write_progress {
                // 进度条
                ui.add(
                    egui::ProgressBar::new(
                        progress.total_progress as f32 / 100.0,
                    )
                    .desired_width(ui.available_width())
                    .text(format!("{}%", progress.total_progress)),
                );

                ui.add_space(5.0);
                ui.label(format!(
                    "步骤: {} ({}/100)",
                    progress.step,
                    progress.step_progress
                ));

                if let Some(ref error) = progress.error {
                    ui.colored_label(
                        egui::Color32::RED,
                        format!("❌ 错误: {}", error),
                    );
                }
            }

            ui.add_space(5.0);
            ui.label(&self.usb_boot_state.status_message);
        }

        // 检查后台进度
        self.check_usb_write_progress();
    }

    /// 刷新 USB 设备列表
    fn refresh_usb_devices(&mut self) {
        self.usb_boot_state.refreshing_devices = true;

        // 在后台线程中执行（USB 检测可能涉及 IOCTL 调用）
        let devices = UsbManager::get_usb_devices();
        self.usb_boot_state.usb_devices = devices;
        self.usb_boot_state.refreshing_devices = false;
        self.usb_boot_state.selected_device = None;

        log::info!(
            "[USB UI] 检测到 {} 个 USB 设备",
            self.usb_boot_state.usb_devices.len()
        );

        if self.usb_boot_state.usb_devices.is_empty() {
            self.usb_boot_state.status_message =
                "未检测到 USB 设备，请插入 U 盘后重试".to_string();
        } else {
            self.usb_boot_state.status_message = format!(
                "检测到 {} 个 USB 设备，请选择一个作为目标",
                self.usb_boot_state.usb_devices.len()
            );
        }
    }

    /// 浏览 ISO 文件
    fn browse_iso_for_usb(&mut self) {
        let file_dialog = rfd::FileDialog::new()
            .add_filter("ISO 镜像文件", &["iso"])
            .add_filter("所有文件", &["*"])
            .set_title("选择 Windows ISO 镜像文件");

        if let Some(path) = file_dialog.pick_file() {
            self.usb_boot_state.iso_path = path.to_string_lossy().to_string();
            self.usb_boot_state.status_message =
                format!("已选择: {}", self.usb_boot_state.iso_path);
        }
    }

    /// 开始 USB 写入
    fn start_usb_write(&mut self) {
        let device_idx = match self.usb_boot_state.selected_device {
            Some(idx) => idx,
            None => {
                self.usb_boot_state.status_message = "请先选择目标 USB 设备".to_string();
                return;
            }
        };

        let device = match self.usb_boot_state.usb_devices.get(device_idx) {
            Some(d) => d.clone(),
            None => {
                self.usb_boot_state.status_message = "选中的设备信息无效".to_string();
                return;
            }
        };

        let iso_path = self.usb_boot_state.iso_path.clone();
        if iso_path.is_empty() || !std::path::Path::new(&iso_path).exists() {
            self.usb_boot_state.status_message = "ISO 文件不存在".to_string();
            return;
        }

        let fs_type = FILE_SYSTEMS[self.usb_boot_state.file_system_idx].1;

        let config = UsbWriteConfig {
            device,
            iso_path,
            partition_scheme: self.usb_boot_state.partition_scheme,
            target_system: TargetSystem::BiosOrUefiCsm,
            file_system: fs_type,
            volume_label: self.usb_boot_state.volume_label.clone(),
            quick_format: self.usb_boot_state.quick_format,
            efi_size_mb: 100,
        };

        // 创建进度通道
        let (tx, rx) = mpsc::channel::<WriteProgress>();

        self.usb_boot_state.is_writing = true;
        self.usb_boot_state.status_message = "正在制作启动盘...".to_string();
        self.usb_boot_state.progress_rx = Some(rx);

        // 在后台线程中执行写入操作
        std::thread::spawn(move || {
            let writer = UsbWriter::new(config).with_progress(tx);
            match writer.write() {
                Ok(_) => {
                    log::info!("[USB UI] 启动盘制作完成");
                }
                Err(e) => {
                    log::error!("[USB UI] 启动盘制作失败: {}", e);
                    // 注意：UsbWriter::write() 失败时已通过 send_error 发送错误
                }
            }
        });
    }

    /// 检查 USB 写入进度
    fn check_usb_write_progress(&mut self) {
        // 先检查是否有 receiver
        let has_rx = self.usb_boot_state.progress_rx.is_some();
        if !has_rx {
            return;
        }
        
        // 非阻塞读取最新进度
        let mut should_clear_rx = false;
        while let Some(ref rx) = self.usb_boot_state.progress_rx {
            match rx.try_recv() {
                Ok(progress) => {
                    if progress.finished {
                        self.usb_boot_state.is_writing = false;
                        should_clear_rx = true;

                        if let Some(ref error) = progress.error {
                            self.usb_boot_state.status_message =
                                format!("❌ 制作失败: {}", error);
                            self.usb_boot_state.write_progress = Some(progress);
                        } else {
                            self.usb_boot_state.status_message =
                                "✅ 启动盘制作完成！".to_string();
                            self.usb_boot_state.write_progress = Some(progress);
                        }
                    } else {
                        self.usb_boot_state.status_message = format!(
                            "正在制作: {} ({}%)",
                            progress.step,
                            progress.total_progress
                        );
                        self.usb_boot_state.write_progress = Some(progress);
                    }
                }
                Err(_) => break,
            }
        }
        
        // 在循环外清理 receiver
        if should_clear_rx {
            self.usb_boot_state.progress_rx = None;
        }
    }
}
