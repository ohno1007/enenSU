# enenSU

在电脑上把选中的 Android 内核模块（`.ko`）写入设备的启动 ramdisk。支持 `boot`、`init_boot`、`vendor_boot` 和 `vendor_kernel_boot`。每次从该镜像开机时，设备已有的 first-stage init 会读取 `modules.load`，在 Android 框架和应用启动前请求内核加载驱动。整个流程无需额外的 root 进程，模块记录、启动日志和签名检查沿用系统原有机制。

## 构建

需要稳定版 Rust 和 C 编译器。刷入时还需要 Android SDK Platform Tools 中的 `fastboot`。

```sh
git clone --recurse-submodules https://github.com/ohno1007/enenSU.git
cd enenSU
cargo build --release --locked
```

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

运行 `cargo test --locked` 可验证四种分区布局、跨镜像模块依赖、板型分片选择、重复修补和 fastboot 分区核对。AOSP 的 `mkbootimg`、`unpack_bootimg` 和 `avbtool` 已用于独立检查修补结果，并用测试密钥验证重新签名的镜像。

以下结果来自公开发布镜像的离线修补和重新解包。验证包括模块字节、依赖、开机加载顺序，以及重复修补的结果。真机启动和具体外设功能仍需设备验证。

| 设备与固件 | 已验证的修补目标 | 实际选择的驱动 |
| --- | --- | --- |
| Pixel 6（oriole），GrapheneOS 2026111800 | `boot`、`vendor_boot` | 从同一版本的 `vendor_dlkm` 加入 USB 串口驱动 `cdc-acm.ko` |
| Pixel 7（panther），GrapheneOS 2026111800 | `init_boot`、`vendor_kernel_boot` | 从同一版本的 `system_dlkm` 加入 `cdc-acm.ko` |
| 一加 11（salami），LineageOS 24.0 20261113 | `init_boot`、`vendor_boot` | 启用镜像已有的 LED 驱动 `leds-qti-tri-led.ko` |

[GrapheneOS 发布记录](https://grapheneos.org/releases#2026111800) · [一加 11 的 LineageOS 构建](https://download.lineageos.org/devices/salami/builds)
