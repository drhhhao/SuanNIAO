# 蒜鸟（SuanNIAO）项目规则

## 名字

- 项目叫 SuanNIAO（蒜鸟），用户不要再看到 PhonePad：电脑端 `SuanNIAO.exe` + `SuanNIAO-core.exe`，数据文件夹 `%APPDATA%\SuanNIAO`，手机包名 `dev.suanniao`，发版文件 `SuanNIAO-PC-版本-Setup.exe`、`SuanNIAO-Android-版本.apk`。
- 新加的名字（文件、互斥量、窗口类、环境变量、临时文件）都用 SuanNIAO / suanniao。PhonePad、phonepad 只留在认改名前文件和换掉旧版的代码里（`LEGACY_*`、`move_legacy_*`、`phonepad/1`、`phonepad-backup`）。
- 手机包名定了就不能再改：改了系统当成另一个 App，别人手机上的旧版升级不上来。
- 电脑端安装程序（`pc/installer/SuanNIAO.iss`）的 `AppId` 也不能改：Windows 靠它认出装过的蒜鸟，改了新版就装成另一个程序、卸载不掉旧的。

## 版本号

- 每次更新只加最后一位：电脑端 0.7.1 → 0.7.2，手机端 0.9.1 → 0.9.2（`versionCode` 同时 +1）。
- 要动中间一位或第一位（比如 0.7.x → 0.8.0，或者 → 1.0.0）之前，先问用户，用户同意了再改。
- 电脑端 `pc/core/Cargo.toml` 和 `pc/ui/Cargo.toml` 的版本一起改：界面发现后台版本比自己旧，就会重启后台。
- 常有几个会话同时改这个目录。改版本号前重新读一遍当前版本，一次部署只加一次。

## GitHub 和发版

- 代码在 GitHub 私有仓库 `drhhhao/SuanNIAO`，分支 `main`。推送前看一眼 `git status`，别把别的会话改了一半的东西带上去。
- 推到 main 以后，`.github/workflows/release.yml` 看「电脑端版本 + 手机端版本」这一对发布过没有：没有就在 GitHub 上编译两端，发一个 Release，名字像 `pc0.7.4-android0.9.3`；发布过就什么都不做。所以只改代码不改版本号，推上去不会出新版。
- 推送就可能发版，属于对外的动作，用户说了再推。
- 手机正式版的签名密钥原件在 `E:\Documents\蒜鸟发版密钥\`，密码在同一个文件夹的 `说明.txt` 里。GitHub 上存的是加密变量 `SUANNIAO_KEYSTORE_BASE64` 和 `SUANNIAO_KEYSTORE_PASSWORD`。密钥绝不能进仓库。
- 正式版和本机 `assembleDebug` 打的包签名不一样。手机上装着 debug 版时，不能直接覆盖装正式版，得先卸载，会丢手机上的设置和语音模型。
- `models/`、`archive/` 不进仓库：模型一个文件 155 MB，超过 GitHub 的上限。手机正式版里带的 x-asr-480ms 是发版时 `release.yml` 从 sherpa-onnx 的发布页下载的（核对指纹）。
- 测试安装包别碰用户正在用的蒜鸟：装到临时目录（`/DIR=`）、不建桌面快捷方式（`/MERGETASKS="!desktopicon"`，桌面上的「蒜鸟」指向 `pc\target\release`）、静默安装不会自动打开（打开新版会把开着的旧版换掉）；开机自启的注册表值先记下，测完对一遍。
