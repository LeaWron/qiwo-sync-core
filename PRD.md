我的目标不是“外部工具配合现有前端”，而是：

```text
直接 fork 各平台成熟 Rime 前端
  ├─ Windows: Weasel / 小狼毫
  ├─ macOS: Squirrel / 鼠须管
  ├─ Android: Trime
  └─ Linux: ibus-rime
然后在里面内置：
  ├─ 默认使用 rime-frost / 白霜拼音
  ├─ WebDAV 同步配置和词库
  ├─ 自动同步间隔
  └─ 自己构建安装，自己使用
```

这个思路**可行**，而且你自己用的话，比做成通用产品简单很多。但仍然建议你不要在每个前端里各写一套 WebDAV。正确做法是：

```text
各平台前端直接魔改
但 WebDAV 同步逻辑仍然抽成一个共用 core
```

也就是：

```text
Weasel fork
  └─ 调用 qiwo-sync-core

ibus-rime fork 
  └─ 调用 qiwo-sync-core
  
Squirrel fork
  └─ 调用 qiwo-sync-core

Trime fork
  └─ 调用 qiwo-sync-core

qiwo-sync-core
  ├─ WebDAV
  ├─ rime-frost 初始化
  ├─ 路径映射
  ├─ 文件过滤
  ├─ 冲突检测
  ├─ 备份
  └─ 同步状态
```

这样你是在**直接改不同平台知名前端**，但不会把同步逻辑重复写三遍。

## 你应该怎么改

### Windows：改 Weasel

Weasel 适合加这些入口：

```text
托盘菜单：
  同步配置
  推送到 WebDAV
  从 WebDAV 拉取
  自动同步：关闭 / 每天 / 每周 / 每月
  WebDAV 设置
  更新白霜拼音
```

内部实现可以是：

```text
WeaselDeployer / WeaselServer / 托盘 UI
  ↓
调用 qiwo-sync-core 或 qiwo-rime-sync.exe
  ↓
同步完成后触发重新部署
```

Windows 这边最适合先做，因为：

```text
托盘菜单好加
路径固定性较强：%APPDATA%\Rime
重新部署入口已有
自己安装测试相对方便
```

你可以先不做复杂 UI，直接在小狼毫托盘菜单里加一个“同步”命令。

---

### macOS：改 Squirrel

Squirrel 可以加到菜单栏输入法菜单里：

```text
同步配置
同步设置
更新雾凇拼音
重新部署
```

macOS 麻烦点在于：

```text
InputMethodKit 生命周期比较怪
输入法进程可能被系统拉起/杀掉
后台网络同步不适合直接塞进输入事件路径
沙盒、权限、钥匙串、登录项都要考虑
```

所以 macOS 上建议：

```text
Squirrel 菜单项
  ↓
启动一个 helper / 调用内置同步模块
  ↓
同步完成
  ↓
reload / deploy
```

不要让 InputController 自己干 WebDAV。输入法控制器应该只管输入。

---

### Android：改 Trime

Trime 适合把同步做进设置页：

```text
设置：
  WebDAV 地址
  用户名
  密码
  同步间隔
  立即同步
  更新雾凇拼音
```

Android 上最好用：

```text
WorkManager 定时同步
设置页手动同步
前台提示同步结果
```

这里不要照搬桌面 CLI。Android 没有那么自然的外部命令调用方式。更适合：

```text
Trime Kotlin/Java
  ↓ JNI / UniFFI / JNA
Rust sync core
```

或者如果你不想引入 Rust 到 Android，也可以先在 Trime 里用 Kotlin 直接实现 WebDAV。  
但长期看，三端共用 core 更稳。

## rime-frost 应该怎么内置

既然你是自己安装自己用，可以直接把 rime-frost 仓库作为前端 fork 的内置资源。

比如每个平台构建产物里带：

```text
resources/rime-frost/
  default.yaml
  rime_frost.schema.yaml
  rime_frost.dict.yaml
  cn_dicts/
  en_dicts/
  opencc/
  lua/
  ...
  (即整个rime_frost仓库)
```

安装/首次启动时：

```text
检测 Rime 用户目录
  ↓
如果没有 rime_frost.schema.yaml
  ↓
复制内置 rime-frost 到用户目录(git clone)
  ↓
写 default.custom.yaml
  ↓
触发部署
```

`default.custom.yaml`：

```yaml
patch:
  schema_list:
    - schema: rime_frost
```

平台配置单独写：

```text
Windows:
  weasel.custom.yaml

macOS:
  squirrel.custom.yaml

linux:
  ibus_rime.custom.yaml

Android:
  trime.yaml / trime.custom.yaml
```

不要直接改 rime-frost 的原始文件。你自己的改动放到：

```text
*.custom.yaml
custom_phrase.txt
平台 custom 配置
```

## WebDAV 同步在前端里怎么放

你可以在每个平台前端里做一层很薄的 adapter。

例如统一抽象：

```rust
pub struct SyncRequest {
    pub frontend: Frontend,
    pub rime_user_dir: PathBuf,
    pub remote_url: String,
    pub username: String,
    pub password: String,
    pub device_id: String,
    pub mode: SyncMode,
}

pub enum Frontend {
    Weasel,
    Squirrel,
    ibus-rime, 
    Trime,
}

pub enum SyncMode {
    Sync,
    Push,
    Pull,
    UpdateRimeIce,
}
```

然后三端只负责传：

```text
我是哪个前端
我的 Rime 用户目录在哪里
WebDAV 配置是什么
用户点了哪个按钮
```

同步 core 负责实际操作。

## 同步哪些文件

自己用也不要直接同步整个 Rime 目录。

建议同步：

```text
*.custom.yaml
*.schema.yaml
*.dict.yaml
custom_phrase.txt
opencc/**
lua/**
symbols.yaml
```

不要同步：

```text
build/**
*.bin
*.table.bin
*.reverse.bin
*.userdb/**
```

用户词库先不要碰 `*.userdb` 原始数据库。更稳的是：

```text
导出用户词库为文本
  ↓
WebDAV 同步文本
  ↓
其他端导入/合并
```

如果第一版你嫌麻烦，可以先只同步配置和自定义词库文件，不同步动态用户词库。自己用也够跑起来。

## 你实际应该按这个顺序做

### 第一步：先 fork 一个平台，建议 Weasel

原因很现实：Windows 端 UI/托盘改起来通常比 macOS InputMethodKit 和 Android JNI 更直接。

第一版只做：

```text
1. 内置 rime-ice
2. 首次启动复制 rime-ice
3. default.custom.yaml 默认 rime_ice
4. 托盘菜单加“同步”
5. 同步 *.custom.yaml / 自定义词库
6. 同步后重新部署
```

不要一开始做多设备冲突合并。

---

### 第二步：把同步逻辑抽出来

哪怕一开始写在 Weasel 里，跑通后也要抽成：

```text
qiwo-sync-core
```

否则你后面改 Squirrel 和 Trime 会重复造轮子。

---

### 第三步：移植到 Squirrel 和 ibus-rime

macOS 只接菜单和路径：

```text
~/Library/Rime
```

linux 同样
```text
~/.config/ibus/rime
```

同步逻辑仍然复用 core。

---

### 第四步：移植到 Trime

Android 最后做，因为它的存储权限、后台任务、JNI/NDK 打包会多一层麻烦。

## 你这个方案的真实难度

按“自己用”来评估：

```text
只改 Weasel + 内置 rime-forst + 手动 WebDAV 同步：
  中等，可做

Weasel + Squirrel +ibus-rime 都支持：
  中等偏难

再加 Trime：
  偏难

三端自动同步、冲突合并、词库安全合并：
  难

做成通用发行产品：
  很难
```

但你自己用，不追求产品化，完全可以降低要求：

```text
配置写死一点
路径写死一点
冲突直接备份
同步失败直接提示日志
密码先本地配置文件保存，后面再接系统钥匙串
```

这就现实很多。

## 最终结论

你这个方向应该这样定：

```text
不是“外部同步器替代前端魔改”
而是“魔改各平台知名前端，但同步能力抽成共用模块”
```

也就是说：

```text
前端你照样 fork：
  Weasel / Squirrel / ibus-rime / Trime

但每个平台只改：
  1. 默认安装 rime-frost
  2. 添加同步设置入口
  3. 添加同步按钮/自动同步定时
  4. 同步后触发重新部署

不要每个平台重复实现：
  WebDAV
  文件过滤
  路径映射
  manifest
  冲突处理
  备份
```


## 统一名字和图标

可以，而且**建议一开始就统一名字和图标**，否则后面 WebDAV 同步的远端目录、设备标识、bundle id、输入源 id、用户目录名都会越来越乱。

你现在的名字看起来是 **齐我输入法 / Qiwo**，可以统一成一套：

```text
中文名：齐我输入法
英文名：Qiwo Input Method
短名：Qiwo
内部名：qiwo
反向域名：com.qiwo.inputmethod
```

## 各平台应该统一哪些东西

### 1. 显示名称

各平台都显示：

```text
齐我输入法
```

或短一点：

```text
齐我
```

建议：

```text
系统输入法列表：齐我输入法
托盘/菜单标题：齐我
进程/包名：QiwoInputMethod / qiwo
```

### 2. 图标

图标最好做一套多尺寸资源，而不是每个平台随便塞一张。

建议源文件：

```text
assets/icon/qiwo.svg
assets/icon/qiwo-1024.png
```

然后生成：

```text
Windows:
  qiwo.ico

macOS:
  qiwo.icns

Linux:
  qiwo.svg
  qiwo.png

Android:
  mipmap-*/ic_launcher.png
  adaptive icon foreground/background
```

你之前做过“齐 / 我”上下结合的图形，这个很适合作为统一图标主体。  
输入法图标建议不要太复杂，因为在菜单栏/托盘/输入源列表里会缩到很小。

## Windows / Weasel 要改哪里

Weasel 里通常要改：

```text
程序名称
托盘图标
安装器显示名
输入法显示名
TSF profile 名称
资源文件 .rc
图标 .ico
注册表里的描述
```

典型文件类型：

```text
*.rc
*.ico
*.iss / installer script
*.wxs / msi 配置，如果有
WeaselServer / WeaselDeployer 相关资源
```

你要搜：

```powershell
rg "Weasel|小狼毫|Rime|rime|weasel"
rg "\.ico|IDI_|ICON"
```

Windows 端特别注意：**TSF 输入法的 GUID / profile id 不要随便改来改去**。  
自己用可以改，但改一次后系统可能残留旧输入法项。建议确定最终名字后再改。

## macOS / Squirrel 要改哪里

macOS 主要改：

```text
Info.plist
.app 名称
.icns 图标
输入源显示名
bundle identifier
TISInputSourceID
InputMethodConnectionName
```

你的 plist 可以统一成：

```xml
<key>CFBundleDisplayName</key>
<string>齐我输入法</string>

<key>CFBundleName</key>
<string>QiwoInputMethod</string>

<key>CFBundleExecutable</key>
<string>QiwoInputMethod</string>

<key>CFBundleIdentifier</key>
<string>com.qiwo.inputmethod</string>

<key>InputMethodConnectionName</key>
<string>QiwoInputMethod_Connection</string>

<key>TISInputSourceID</key>
<string>com.qiwo.inputmethod</string>
```

输入模式：

```xml
<key>com.qiwo.inputmethod.rime</key>
<dict>
  <key>TISInputSourceID</key>
  <string>com.qiwo.inputmethod.rime</string>
  <key>TISIconLabels</key>
  <dict>
    <key>Primary</key>
    <string>齐我</string>
  </dict>
</dict>
```

这里建议你别继续用：

```text
com.qiwo.inputmethod.QiwoInputMethod.QiwoKey
```

太长，也不优雅。  
建议改成：

```text
com.qiwo.inputmethod.rime
```

或者：

```text
com.qiwo.inputmethod.qiwo
```

macOS 改名字后一定要清缓存，否则你会看到旧名字、旧图标、旧输入源残留。

```fish
rm -rf ~/Library/Input\ Methods/QiwoInputMethod.app
killall QiwoInputMethod 2>/dev/null
killall cfprefsd 2>/dev/null
killall TextInputMenuAgent 2>/dev/null
killall SystemUIServer 2>/dev/null
```

必要时注销重登。

## Linux / ibus-rime 要改哪里

ibus-rime 主要改：

```text
engine 名称
engine longname
icon
desktop 文件
ibus component XML
安装路径下的 icon
```

一般会有类似：

```text
ibus-engine-rime
rime.xml
ibus-rime.desktop
icons/rime.svg
```

你可以改成：

```xml
<name>qiwo</name>
<longname>齐我输入法</longname>
<description>Qiwo Input Method</description>
<language>zh_CN</language>
<icon>/usr/share/icons/hicolor/scalable/apps/qiwo.svg</icon>
```

Linux 端建议：

```text
engine name: qiwo
display name: 齐我输入法
binary name: ibus-engine-qiwo
icon name: qiwo
```

但有个现实问题：  
如果你只是 fork ibus-rime，然后把 engine name 从 `rime` 改成 `qiwo`，你要同步改 systemd/dbus/ibus component 注册相关字段。可以改，但要改完整。

自己用的话，最稳的是：

```text
内部 engine 仍然叫 rime
显示名和图标改成 Qiwo/齐我
```

这样不容易破坏 IBus 注册。

## Android / Trime 要改哪里

Android 改名主要是：

```text
app_name
applicationId
launcher icon
IME subtype label
settings title
notification title
```

典型位置：

```text
app/src/main/res/values/strings.xml
app/build.gradle
AndroidManifest.xml
res/xml/method.xml
res/mipmap-*/
```

建议：

```xml
<string name="app_name">齐我输入法</string>
<string name="ime_name">齐我输入法</string>
```

包名：

```text
com.qiwo.inputmethod
```

但注意：如果你 fork Trime 并改 `applicationId`，它会被 Android 当成一个全新输入法。  
这是你想要的，适合自己安装自己的版本。

## 统一命名建议

我建议最终定成：

```text
产品名：
  齐我输入法

英文名：
  Qiwo Input Method

短名：
  Qiwo

命令行工具：
  qiwo-rime-sync

同步核心：
  qiwo-sync-core

Windows exe：
  QiwoInputMethod.exe
  QiwoServer.exe
  QiwoDeployer.exe

macOS app：
  QiwoInputMethod.app

Linux engine：
  ibus-engine-qiwo
  或显示名改 Qiwo，内部仍用 ibus-engine-rime

Android package：
  com.qiwo.inputmethod
```

## 图标资源组织

你的总仓库或同步 core 仓库里可以建：

```text
assets/
  icon/
    qiwo.svg
    qiwo-1024.png
    windows/
      qiwo.ico
    macos/
      QiwoInputMethod.icns
    linux/
      qiwo.svg
      qiwo.png
    android/
      foreground.svg
      background.xml
      mipmap-hdpi/
      mipmap-mdpi/
      mipmap-xhdpi/
      mipmap-xxhdpi/
      mipmap-xxxhdpi/
```

源头只维护：

```text
qiwo.svg
qiwo-1024.png
```

其他格式用脚本生成。

## 最需要注意的坑

### 1. 名字可以经常改，ID 不要经常改

显示名：

```text
齐我输入法
```

可以改。

但这些尽量确定后别再改：

```text
com.qiwo.inputmethod
com.qiwo.inputmethod.rime
Windows TSF GUID
Android applicationId
IBus engine name
```

因为系统会缓存输入法身份。

### 2. 图标路径要和 plist/xml/rc 完全一致

macOS 里：

```xml
<key>CFBundleIconFile</key>
<string>qiwo.icns</string>
```

那文件就要真的在：

```text
Contents/Resources/qiwo.icns
```

不要写 `qi.tiff` 但实际放的是 `qiwo.png`。

### 3. 平台前端不要共用同一个配置文件名

可以统一品牌，但配置还是分平台：

```text
weasel.custom.yaml
squirrel.custom.yaml
trime.yaml
ibus_rime.custom.yaml
```

## 推荐你现在这么做

先定品牌常量：

```text
QIWO_DISPLAY_NAME_ZH = 齐我输入法
QIWO_DISPLAY_NAME_SHORT = 齐我
QIWO_APP_ID = com.qiwo.inputmethod
QIWO_SCHEMA_DEFAULT = rime_frost
QIWO_SYNC_DIR = qiwo-rime-sync
```

然后所有平台 fork 里都围绕这套常量改。

最终效果：

```text
系统输入法列表：齐我输入法
菜单栏/托盘图标：统一 Qiwo 图标
默认方案：白霜拼音 rime_frost
同步入口：齐我同步
远端目录：/qiwo-rime-sync/
```

2. 输入状态图标

这个才是你说的“中英文指示器”。

Windows 端通常会有类似状态图标资源：

中文状态图标
英文状态图标
禁用状态图标
全角状态图标
半角状态图标
ASCII mode 图标

显示效果大概是：

中
英
A
あ
繁
简

你如果想做统一品牌，不建议把这些都换成同一个 Qiwo 主图标。否则状态看不出来。

更合理的是：

主图标：齐我 logo
中文状态：中
英文状态：英
禁用状态：灰色 Qiwo
全角：全
半角：半

也就是说，品牌图标和状态图标应该同一风格，但内容不同。

推荐的 Windows 图标集

你可以准备这一组：

assets/windows/icons/
  qiwo.ico                  # 主图标
  qiwo_tray.ico             # 托盘默认图标

  chi*.ico        # 中文输入状态
  eng*.ico        # 英文/ASCII 状态
  *_capson.ico  # 禁用/不可用
  *_full_*.ico      # 全角
  *_half_*.ico      # 半角