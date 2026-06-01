# 平台仓库

父仓库只保留产品文档、资源说明和平台仓库指针。平台实现已经拆分到独立 private 仓库，并在这里以 submodule 方式引用。

## Submodules

| 平台 | 路径 | 仓库 | 基础项目 |
| --- | --- | --- | --- |
| Windows | `qiwo-weasel` | `LeaWron/qiwo-weasel` | weasel |
| Linux | `qiwo-ibusr` | `LeaWron/qiwo-ibusr` | ibus-rime |
| macOS | `qiwo-squirrel` | `LeaWron/qiwo-squirrel` | squirrel |
| Android | `qiwo-trime` | `LeaWron/qiwo-trime` | trime |
| 同步核心 | `qiwo-sync-core` | `LeaWron/qiwo-sync-core` | .NET |

## 使用方式

首次克隆父仓库:

```powershell
git clone --recurse-submodules https://github.com/LeaWron/myime.git
```

已有工作区初始化或更新 submodule:

```powershell
git submodule update --init --recursive
git submodule update --remote --merge
```

## 依赖边界

`librime`、`weasel`、`ibus-rime`、`squirrel`、`trime`、`plum`、`Sparkle`、`rime-frost` 等平台依赖由各平台仓库自行管理。

`qiwo-sync-core` 是 Windows 和 macOS 当前共用的同步核心；Linux 当前在 `qiwo-ibusr` 内维护 Python 版 WebDAV 同步脚本。
