# enenSU

在电脑上把选中的 Android 内核模块（`.ko`）写入 `boot.img` 的 ramdisk，也支持 `vendor_boot.img`。开机时由设备已有的 first-stage init 读取公开的 `modules.load`，在 Android 框架和应用启动前加载驱动。工具只添加模块文件及其加载清单，设备无需安装 `su`、`ksud`、替代 `init` 或后台服务。内核的模块记录、日志及签名策略均按设备原有机制工作。

## 构建

需要稳定版 Rust 和 C 编译器。刷入时还需要 Android SDK Platform Tools 中的 `fastboot`。

```sh
git clone --recurse-submodules https://github.com/ohno1007/enenSU.git
cd enenSU
cargo build --release --locked
```

## 把模块写入 boot

准备目标设备同一系统版本的原厂镜像，以及针对该内核编译的 `.ko`。下面的命令把 `sensor.ko` 写入新生成的 `boot-patched.img`。配套的 `vendor_boot.img` 以只读方式提供厂商原有的模块顺序和依赖。若两份清单都已在 `boot.img` 自己的 ramdisk 中，可以省略 `--vendor-boot`。

```sh
./target/release/enensu patch boot.img -o boot-patched.img \
  --module ./sensor.ko --vendor-boot vendor_boot.img
```

多选时重复使用 `--module`。镜像中已收录的模块可以按名称启用，例如 `--enable sensor`。工具读取模块的 ELF 元数据，按需更新 `modules.load`、`modules.dep` 和 `modules.softdep`。同名替换需保持模块名、架构、`vermagic` 和依赖兼容。`.ko` 的签名字节随文件一同写入。不选模块或重复应用相同选择时，输出与输入逐字节一致。输出路径必须是新文件。

### 镜像布局

- 支持 `boot` 和 `vendor_boot` v3/v4。直接修补 `boot` 需要其中既有内核又有 ramdisk，以及设备的标准 `modules.load` 和 `modules.dep`。驱动的实际加载发生在 Linux 启动 `/init` 之后、Android 框架启动之前。
- 部分设备把通用 ramdisk 放在 `init_boot`，这时 `boot` 只含内核。对于这种布局，可把同一模块写入设备原有的 `vendor_boot` 模块目录。
- `vendor_boot` v4 默认选择唯一的 DLKM 分片，其次选择唯一可用于正常启动的分片。多个候选时用 `--fragment 名称` 指定该设备的模块分片。存在多个内核版本目录时，用 `--module-dir lib/modules/6.6` 指定实际加载目录。

```sh
./target/release/enensu patch vendor_boot.img -o vendor_boot-patched.img --module ./sensor.ko
```

## 刷入和还原

内容修改后得到待签名的镜像载荷。使用 Android Verified Boot（AVB）的构建应通过设备原有流程重新签名并更新相关 vbmeta 描述符。使用 v4 GKI 启动证书的构建也应重新生成证书。以下示例中的 `boot-final.img` 是该设备可接受的最终镜像。

在设备的 fastboot 模式下，把 `SERIAL` 换成 `fastboot devices` 显示的序列号。工具会核对镜像格式与目标分区，槽位由 fastboot 按设备配置选择。

```sh
./target/release/enensu flash --partition boot --serial SERIAL boot-final.img
```

刷入厂商镜像时使用 `--partition vendor_boot`。要撤回模块选择，可在同一系统版本和槽位刷回保留的原厂镜像，并还原签名流程中同步更新的 vbmeta 镜像。

## 上游与验证

`upstream/KernelSU` 是固定在 v3.3.1（`0c0fdb9`）的 Git 子模块。`src/image.rs` 改编自上游的 `userspace/ksud/src/boot_patch.rs`，沿用同一版本的 `android-bootimg` 解包和重打包 ramdisk。主机端 `enensu` 是本仓库的构建入口。[KernelSU 原始实现](https://github.com/tiann/KernelSU/blob/0c0fdb960178fa12d80d6cbcfdcb6ad8be6a865d/userspace/ksud/src/boot_patch.rs) · [Android 模块加载机制](https://source.android.com/docs/core/architecture/kernel/kernel-module-support) · [许可证](LICENSE)

运行 `cargo test --locked` 可验证两种镜像布局、原厂加载清单的保留、重复修补、签名处理和 fastboot 分区选择。
