# enenSU

在电脑上把选中的 Android 内核模块（`.ko`）写入设备的启动 ramdisk。支持 `boot`、`init_boot`、`vendor_boot` 和 `vendor_kernel_boot`。每次从该镜像开机时，设备已有的 first-stage init 会读取 `modules.load`，在 Android 框架和应用启动前请求内核加载驱动。整个流程无需额外的 root 进程，模块记录、启动日志和签名检查沿用系统原有机制。

## 从电脑自动刷入

### Windows 图形应用

下载 Windows x64 的 `enenSU-Desktop.exe`，双击即可使用。Material 3 界面、刷入引擎、ADB 和 fastboot 都已内置。Windows 10/11 使用系统 WebView2 渲染界面，缺少运行时时应用会提示联网安装微软运行时。手机需要已解锁 bootloader、开启 USB 调试并授权这台电脑，Windows 需要能识别该手机的 ADB 和 fastboot USB 驱动。[下载 Windows 图形应用](https://github.com/ohno1007/enenSU/releases/tag/v0.3.0)

1. 用 USB 连接手机。设备卡片会显示授权状态、型号、内核和当前槽位。连接多台手机时，在卡片里选定目标。
2. 点击「选择模块」，选一个或多个与该内核匹配的 `.ko`。启动镜像默认从设备当前槽位回读。能回读时只需选择模块，无需准备 ROM 包。也可以预先选定同机型、同固件版本的原始 `.img`。
3. 点击「修补并刷入」。进度图依次显示设备检查、镜像修补、分区写入和开机验证，并记录实际镜像来源。应用先尝试普通 ADB 读取可读分区，再尝试设备支持的 bootloader fastboot 或 fastbootd 回读。两个接口都不提供所需镜像时，窗口会显示设备构建指纹并请你选择原始镜像。[Android fastboot 回读条件](https://source.android.com/docs/core/architecture/bootloader/fastbootd)

应用在写入前保存源镜像和修补镜像，位置为 `%LOCALAPPDATA%\enenSU\images\install-…\`。点击「打开备份」可以定位本次文件。「查看日志」显示实际分区、工作阶段和设备返回的错误。开机后 ADB 无权读取模块状态时，界面会显示「模块加载待验证」。

### 命令行刷入

Linux x64 可下载命令行版，在解压目录运行 `./enensu install`。电脑的 `PATH` 中需要有 Android SDK Platform Tools 的 `adb` 和 `fastboot`。Linux 的文件选择窗口使用 Zenity 或 KDialog。[下载 Linux 命令行版](https://github.com/ohno1007/enenSU/releases/tag/v0.2.0) · [Android SDK Platform Tools](https://developer.android.com/tools/releases/platform-tools)

已有同版本固件目录时，可以从命令行完成整个流程。可重复使用 `--module` 选择多个驱动。连接多台手机时，用 `--serial` 指定 `adb devices` 中的设备。

```sh
./enensu install --module ./sensor.ko --firmware ./factory-images
```

目录模式优先选择持有正常启动模块清单的 `vendor_kernel_boot` 或 `vendor_boot`，也支持 `init_boot` 和 `boot`。通过 `--firmware ./init_boot.img` 或 `--partition init_boot` 可以明确指定通用 ramdisk。固件有多个板型分片时可用 `--fragment`，同时使用两份厂商参考镜像时可用 `--vendor-boot-fragment`。工具会比对镜像提供的 AVB 构建指纹和手机报告的指纹。没有可比对的指纹时，会提示使用者核对所选固件版本。

命令行版会把源镜像和修补镜像保存在电脑的 `enensu-images/install-…/` 中。用 `--output-dir` 可选择保存位置。回读得到的 `*-source.img` 是当时的分区内容，手动选择固件时它是所选固件的副本。fastboot 报告写入失败时，工具保留镜像并停止，方便在对应槽位恢复。自动刷入适用于解锁后允许启动修补镜像的设备。仍要求重新签名的设备请使用下方的离线修补、设备签名和刷写流程。

## 构建

需要稳定版 Rust 和 C 编译器。桌面文件选择窗口使用 tinyfiledialogs。Linux 可以使用 Zenity 或 KDialog，也可以用命令行参数指定文件。[tinyfiledialogs](https://github.com/jdm/tinyfiledialogs-rs)

```sh
git clone --recurse-submodules https://github.com/ohno1007/enenSU.git
cd enenSU
cargo build --release --locked
```

从 Windows x64 的 Visual Studio 开发者终端构建图形应用，需要 Rust MSVC 工具链、C/C++ 编译器、Node.js/npm 和 Python 3。准备脚本会校验固定版本的 Platform Tools 和 WebView2 引导程序，并把 Material Web 组件、字体和开源许可打包为本地资源。

```powershell
python desktop/prepare.py
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked --target x86_64-pc-windows-msvc --features desktop --bin enenSU-Desktop
```

生成的 `target/x86_64-pc-windows-msvc/release/enenSU-Desktop.exe` 可以单独分发。界面使用 Google Material Web 的卡片、按钮、设备选择框、对话框和进度指示器，并跟随 Windows 浅色或深色主题。[Material Web](https://material-web.dev/) · [Microsoft WebView2 Runtime](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution)

## 选择要修补的镜像

从**同一设备、同一固件版本**取出镜像，根据正常开机时使用的 ramdisk 选择目标。模块需要匹配目标内核的架构、模块 ABI、依赖和签名策略。

| 目标分区 | 可修补的布局 |
| --- | --- |
| `boot` | Android v3/v4，内核与通用 ramdisk 在一起 |
| `init_boot` | Android v4，通用 ramdisk 独立，`boot` 只放内核 |
| `vendor_boot` | VNDRBOOT v3/v4，正常启动的厂商 ramdisk 内含模块目录 |
| `vendor_kernel_boot` | 独立的厂商驱动 ramdisk，使用 VNDRBOOT v3/v4 格式 |

修补通用 ramdisk 时，配套的厂商镜像以只读方式提供既有的模块顺序和依赖。如果 `modules.load` 和 `modules.dep` 都已在目标 ramdisk 里，可以省略参考镜像。

```sh
./target/release/enensu patch boot.img -o boot-patched.img \
  --module ./sensor.ko --vendor-boot vendor_boot.img
```

如果通用 ramdisk 位于 `init_boot`，按固件实际提供的分区传入参考镜像。下面的示例适用于具备独立 `vendor_kernel_boot` 的布局。

```sh
./target/release/enensu patch init_boot.img -o init_boot-patched.img \
  --module ./sensor.ko --vendor-boot vendor_boot.img --vendor-kernel-boot vendor_kernel_boot.img
```

也可以直接修补**持有正常启动模块清单**的厂商分区。以下示例分别对应两种布局，选其中一条即可。

```sh
./target/release/enensu patch vendor_boot.img -o vendor_boot-patched.img --module ./sensor.ko
./target/release/enensu patch vendor_kernel_boot.img -o vendor_kernel_boot-patched.img --module ./sensor.ko
```

两个厂商分区都支持 v4 ramdisk 分片。工具优先选唯一的 DLKM 分片，其次选唯一的正常启动分片。多个候选时用 `--fragment 名称` 选择对应板型的分片。一起传入两份参考镜像时，`--fragment` 选择 `vendor_kernel_boot` 的分片，`--vendor-boot-fragment` 选择 `vendor_boot` 的分片。存在多个内核版本目录时，用 `--module-dir lib/modules/6.6` 指定该设备实际加载的目录。

多选时重复使用 `--module`。镜像中已收录的模块可以按名称启用，例如 `--enable sensor`。工具读取模块的 ELF 元数据，按需更新 `modules.load`、`modules.dep` 和 `modules.softdep`。同名替换需保持模块名、架构、`vermagic` 和依赖兼容。`.ko` 的签名字节随文件一同写入。不选模块或重复应用相同选择时，输出与输入逐字节一致。输出路径必须是新文件。

## 刷入和还原

内容修改后得到待签名的镜像载荷。使用 Android Verified Boot（AVB）的构建应通过设备原有流程重新签名并更新相关 vbmeta 描述符。使用 v4 GKI 启动证书的构建也应重新生成证书。以下示例中的 `boot-final.img` 是该设备可接受的最终镜像。

在设备支持刷写的 fastboot 模式下，把 `SERIAL` 换成 `fastboot devices` 显示的序列号。`--partition` 填写源镜像所属的分区。工具核对镜像头和可用的 AVB 分区标签。两个厂商分区的镜像头相同，缺少标签时会参考标准文件名。只有能识别为 `vendor_boot` 的镜像才默认选取该分区。槽位由 fastboot 按设备配置选择。

```sh
./target/release/enensu flash --partition boot --serial SERIAL boot-final.img
```

锁定 bootloader 的设备需要它信任的启动链及厂商允许的安装通道。镜像签名和内核对 `.ko` 的签名检查是两个独立条件。[Android Verified Boot 设备状态](https://source.android.com/docs/security/features/verifiedboot/device-state)

重启会继续使用当前槽位的模块清单。OTA 更换启动镜像或切换到另一套固件后，要基于新版本的镜像和匹配的模块重新修补。要撤回模块选择，可在同一版本和槽位刷回保留的原始镜像，并还原签名流程中同步更新的 vbmeta 镜像。

## 上游与验证

`upstream/KernelSU` 是固定在 v3.3.1（`0c0fdb9`）的 Git 子模块。`src/image.rs` 改编自上游的 `userspace/ksud/src/boot_patch.rs`，沿用同一版本的 `android-bootimg` 解包和重打包 ramdisk。主机端 `enensu` 是本仓库的构建入口。[KernelSU 原始实现](https://github.com/tiann/KernelSU/blob/0c0fdb960178fa12d80d6cbcfdcb6ad8be6a865d/userspace/ksud/src/boot_patch.rs) · [Android 模块加载机制](https://source.android.com/docs/core/architecture/kernel/kernel-module-support) · [许可证](LICENSE)

运行 `cargo test --locked` 可验证四种分区布局、跨镜像模块依赖、板型分片选择、重复修补和 fastboot 分区核对。Unix 上的自动刷入测试使用模拟 ADB 和 fastboot，覆盖读取镜像、只写当前槽位、解锁状态与固件指纹不符、刷写失败、槽位回退和开机后的加载检查。AOSP 的 `mkbootimg`、`unpack_bootimg` 和 `avbtool` 已用于独立检查修补结果，并用测试密钥验证重新签名的镜像。

以下结果来自公开发布镜像的离线修补和重新解包。验证包括模块字节、依赖、开机加载顺序，以及重复修补的结果。真机启动和具体外设功能仍需设备验证。

| 设备与固件 | 已验证的修补目标 | 实际选择的驱动 |
| --- | --- | --- |
| Pixel 6（oriole），GrapheneOS 2026111800 | `boot`、`vendor_boot` | 从同一版本的 `vendor_dlkm` 加入 USB 串口驱动 `cdc-acm.ko` |
| Pixel 7（panther），GrapheneOS 2026111800 | `init_boot`、`vendor_kernel_boot` | 从同一版本的 `system_dlkm` 加入 `cdc-acm.ko` |
| 一加 11（salami），LineageOS 24.0 20261113 | `init_boot`、`vendor_boot` | 启用镜像已有的 LED 驱动 `leds-qti-tri-led.ko` |

[GrapheneOS 发布记录](https://grapheneos.org/releases#2026111800) · [一加 11 的 LineageOS 构建](https://download.lineageos.org/devices/salami/builds)
