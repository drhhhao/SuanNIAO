# 蒜鸟（SuanNIAO）项目规则

## 名字

- 项目叫 SuanNIAO（蒜鸟），用户不要再看到 PhonePad：电脑端 `SuanNIAO.exe` + `SuanNIAO-core.exe`，数据文件夹 `%APPDATA%\SuanNIAO`，手机包名 `dev.suanniao`，发版文件 `SuanNIAO-PC-版本-Setup.exe`、`SuanNIAO-Android-版本.apk`。
- 新加的名字（文件、互斥量、窗口类、环境变量、临时文件）都用 SuanNIAO / suanniao。PhonePad、phonepad 只留在认改名前文件和换掉旧版的代码里（`LEGACY_*`、`move_legacy_*`、`phonepad/1`、`phonepad-backup`）。
- 手机包名定了就不能再改：改了系统当成另一个 App，别人手机上的旧版升级不上来。
- 电脑端安装程序的 `AppId`（`pc/ui/src/setup.rs` 的 `APP_ID`，和 0.7.6 的 Inno Setup 安装包一样）也不能改：Windows 靠它认出装过的蒜鸟，改了新版就装成另一个程序、卸载不掉旧的。

## 版本号

- 每次更新只加最后一位：电脑端 0.7.1 → 0.7.2，手机端 0.9.1 → 0.9.2（`versionCode` 同时 +1）。
- 要动中间一位或第一位（比如 0.7.x → 0.8.0，或者 → 1.0.0）之前，先问用户，用户同意了再改。
- 电脑端 `pc/core/Cargo.toml` 和 `pc/ui/Cargo.toml` 的版本一起改：界面发现后台版本比自己旧，就会重启后台。
- 常有几个会话同时改这个目录。改版本号前重新读一遍当前版本，一次部署只加一次。

## 更新说明

- 每次更新版本，都要说明这一版比上一版改了哪些内容（用户要求）。两处都要有：
  - 在 `CHANGELOG.md` 最上面加一段，标题「## 电脑端 X · 手机端 Y」和版本号一字不差，分电脑端、手机端写，用大白话写给用的人看；哪一端没改就写「没有变化」。发版时 `release.yml` 把这一段放进 GitHub Release 的说明，找不到这段就不发。
  - 部署或发版后给用户的回复里，也列出这一版相对上一版改了什么。
- 技术细节写在 `DEVELOPMENT.md` 的「当前版本」下面，和 `CHANGELOG.md` 不冲突。
- `README.md` 是给用的人看的（这个项目干什么、功能、下载安装、使用教程、常见问题），不写技术细节；功能或用法变了要跟着改（用户 2026-10-30 要求）。

## GitHub 和发版

- 代码在 GitHub 公开仓库 `drhhhao/SuanNIAO`（公开是为了两端不登录就能检查更新、下载安装包），分支 `main`。推送前看一眼 `git status`，别把别的会话改了一半的东西带上去。
- 推到 main 以后，`.github/workflows/release.yml` 看「电脑端版本 + 手机端版本」这一对发布过没有：没有就在 GitHub 上编译两端，发一个 Release，名字像 `pc0.7.4-android0.9.3`；发布过就什么都不做。所以只改代码不改版本号，推上去不会出新版。
- 推送就可能发版，属于对外的动作，用户说了再推。发版后装着的蒜鸟（电脑、手机）都会读到 Release 里的 `latest.json`、提示用户更新，所以推之前要确认这一版能用。
- 手机正式版的签名密钥原件在 `E:\Documents\蒜鸟发版密钥\`，密码在同一个文件夹的 `说明.txt` 里。GitHub 上存的是加密变量 `SUANNIAO_KEYSTORE_BASE64` 和 `SUANNIAO_KEYSTORE_PASSWORD`。密钥绝不能进仓库。
- 正式版和本机 `assembleDebug` 打的包签名不一样。手机上装着 debug 版时，不能直接覆盖装正式版，得先卸载，会丢手机上的设置和语音模型。
- `models/`、`archive/` 不进仓库：模型一个文件 155 MB，超过 GitHub 的上限。手机正式版里带的 x-asr-480ms 是发版时 `release.yml` 从 sherpa-onnx 的发布页下载的（核对指纹）。
- 测试安装包别碰用户正在用的蒜鸟：设环境变量 `SUANNIAO_SETUP_SANDBOX=临时文件夹`（开始菜单、桌面、注册表都换到测试用的地方，不打开装好的程序，窗口放在屏幕外面），再用 `--dir` 装到临时文件夹里。桌面上的「蒜鸟」指向 `pc\target\release`，不能被测试盖掉；开机自启的注册表值先记下，测完对一遍。测检查更新用 `SUANNIAO_UPDATE_URL` 指到本机起的 http 服务，别对着真的 GitHub 测下载安装。
