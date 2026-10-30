# 蒜鸟开发说明

给改代码、发版的人看：每一版的技术细节、目录、编译、发版、通信格式、验证记录。怎么用蒜鸟见 [README.md](README.md)，每一版给用户看的更新说明见 [CHANGELOG.md](CHANGELOG.md)。

> 英文名 SuanNIAO，原名 PhonePad。电脑端 0.7.5、手机端 0.9.4 起，exe、数据文件夹、手机包名这些内部名字也都改成了 SuanNIAO；PhonePad 只留在认旧版文件、换掉旧版的代码里。

把小米 13 变成电脑的触控板、快捷键面板和语音输入。手机和电脑通过蓝牙连接；语音在手机上识别（本地模型，不用联网），只把文字发给电脑。方案见上一级目录的 `方案.md`。

## 当前版本：电脑端 0.7.20 · 手机端 0.9.19

- 手机端 0.9.19：自己的扫码界面 `ScanActivity.kt`（用户 2026-10-30 定的样子，效果图在 `xiaomi13\界面打板\扫码界面\`；按推荐：保留模式小标签、手电筒在右上角、认出来后停一下再退）。替换掉 zxing-android-embedded 的 `CaptureActivity`（清单里那条 `fullSensor` 覆盖删了，`IntentIntegrator` 不再用），库只用 `BarcodeView`（`DefaultDecoderFactory(QR_CODE)` + `decodeContinuous`）。
  - 界面：`Layout`（FrameLayout）按取景框摆控件，取景框宽 = 屏宽 × 0.672、顶边在屏高 0.317 处，`setFramingRectSize` 同步给 zxing 只认框里的；`ScanOverlay` 按时间画遮罩（EVEN_ODD 挖圆角框）、四个角（顺着 28dp 圆角的弧画）、光带（1.8 秒一趟，clip 在框里）、认出来的闪光和对勾（`PathMeasure` 一笔画出），`running` 时 `postInvalidateOnAnimation`；图标是 Lucide 路径（`PathParser`）。
  - 状态：进场（遮罩 220 ms 变暗、四个角 1.16 → 1 软回弹、控件上浮淡入）；认出蒜鸟的码（`LanQr.parse` 过）→ `stopDecoding`、CONFIRM 震、提示换成电脑名 +「正在配对 / 绑定 / 连接…」（按 `EXTRA_MODE`）、1.1 秒后退；别的码 → 抖 360 ms、四角变白、REJECT 震、1.5 秒恢复，同一个码 1.5 秒内不重复抖；8 秒没结果换提示。手电筒按钮只在有闪光灯时出现，`onPause` 关掉。
  - 结果：`MainActivity.scanComputer` → `startActivityForResult(REQ_SCAN = 8)`，带 `mode`（bt / lan / any；ANY 时蓝牙没准备好按 lan 显示）和小标签字；回来 `text` → `onScanned`，`album` → `pickQrImage()`。进出用窗口动画（`res/anim/scan_enter` 淡入 160 ms、`scan_exit` 淡出 + 放大 1.04 260 ms）：在自己窗口里淡会先变黑再跳回浅色主界面。主题 `SuanNiao.Scan`（深色、透明系统栏）。
  - 相机预览改用 SurfaceView（`isUseTextureView = false`，库默认 TextureView）：相机画面由系统单独一层合成。
  - 刷新率：扫码时 `dumpsys display` renderFrameRate 120，窗口投票 120 Exact。framestats 里每 4 个刷新周期出 3 帧（约 90 帧/秒），原因是 RenderThread 的 `eglSwapBuffers` 每帧阻塞约 8.6 ms——主界面（菜单动画）一样是 8.6 ms，关掉识别、换 SurfaceView 都不变，是这台手机上整个 App 的现状，不是扫码界面的问题（以后可以单独查）。
  - 调试：`--es debugScanFake 内容` 打开扫码界面（蓝牙配对模式），1.5 秒后当成扫到了这段内容，只看动画、不真去连（认出来以后按取消返回）。录屏看动画：`adb shell screenrecord --time-limit 5`，再用 imageio_ffmpeg 自带的 ffmpeg 抽帧拼图（本机 PATH 里没有 ffmpeg）。
  - 没测到：真对着电脑屏幕扫（手机镜头贴着桌面，画面是黑的；相机在出帧，SurfaceView 帧号在涨）。

- 手机端 0.9.18：重新扫码绑定局域网（同一台电脑，带 `prev`，电脑把旧编号换成新的）时，`lanLoop` 手上正在连的那次还拿着旧编号 → 电脑回 `unknown` → 以前当成「电脑上解除了绑定」、弹提示（新绑定没被改成 revoked，只是误报）。现在 `LanAuthException` 时先看存着的绑定是不是同一台电脑、编号变了、没 revoked：是就记一行 lan.log、`lanKicks` 马上用新的重连，不提示。用 `btpairtest.py lan` 在已绑定的手机上重扫复现过，35 ms 后连上。
- 电脑端 0.7.20：设置卡片「局域网连接」→「扫码连接」（行「扫码连接手机」/「蓝牙配对、局域网绑定都扫这一个码」），手机列表离线时写「局域网 · 上次连接」；首页 `LanCheck`、教程、手机 `NOT_OUR_QR`、解除局域网绑定的说明、手机教程局域网页第 1 步、两端教程插图里的窗口名都改成「扫码连接」/「扫码连接手机」。

- 电脑端 0.7.19：电脑教程（`TutorialDialog`）页号改成 0–11：0 欢迎、1 装好、2 选方式、3 让手机扫码配对（新，插图 `ScanPairStage`）、4 系统蓝牙配对、5 选电脑（4、5 是蓝牙的备选，`manual` 为真时走）、6 扫码走局域网、7 布局、8 映射、9 怎么用、10 托盘、11 准备好了；`main.rs` `TUTORIAL_TRAY_PAGE = 10`、`TUTORIAL_LAST_PAGE = 11`。蓝牙扫码路 9 步、备选路 10 步、局域网路 9 步（`step-of` / `page-of` 三套换算），`use-manual(bool)` 换路并停在新路的配对页，页底链接是 `TutLink`。`--tutorial 3` 走扫码配对，`4`/`5` 走备选，`6` 走局域网。第 3 页第二项检查用新属性 `bt-online`（`ui/src/lan.rs` 按 `status.links` 里有没有 `bt` 设）。
- 手机端 0.9.17：教程的蓝牙路改成扫码配对为主（用户 2026-10-30 按推荐定：配对 + 选电脑合成一页、扫不了码换回原来两页、显示「正在配对…」）。
  - `Tutorial.kt`：新页 `btScanPage`（`BtScanScene` 动画：扫码 → 系统「蓝牙配对请求」浮上来、手指点「配对」→ 电脑冒出「自动同意」→ 换成蒜鸟、连线变实线挂「蓝牙」、二维码打勾），状态卡 `btScanCard`：没权限 `permCard` / `host.btPairing()` 转圈（`SpinBadge`，没按钮）/ 选好的电脑已配对 + `host.btConnected()`「已用蓝牙连上」/ 已配对 / 没配对「扫码」（`ScanFor.BT`）。`btManual`：页底「扫不了码？改用系统蓝牙配对 ›」→ `route()` 换成 `pairPage` + `pickPage`（10 页），配对页底「想省事：改回扫码配对 ›」换回来，`useManual` 原地重排、停在同一页号。每次打开教程 `btManual = false`。二维码画法抽成文件级 `qrModules()`，`LanScene` 和 `BtScanScene` 共用。
  - `Setup.Host` 加 `btPairing()`（`BtPair.busy`）、`btConnected()`（`LinkService.btConnected`）；`pairBluetooth` 发起配对和结束时都 `refreshOnboarding()`。
  - 改字：选择页蓝牙「接下来：扫一下电脑上的二维码，配上蓝牙」，最后一页局域网路「没有网也想用：⋯ → 选择电脑 → 扫码配对新电脑。」
  - 手机上看过：新页三种状态里的「已用蓝牙连上」、动画各帧、改用 / 改回切换（圆点 9 ↔ 10）。没看到真的「正在配对…」（要从头配对一次）。

- 电脑端 0.7.18 / 手机端 0.9.16：扫码蓝牙配对。
  - 二维码多一个 `bt=电脑蓝牙地址`（`pc/ui/src/lan.rs` `payload`，地址来自 `status.lan.btAddr`）；电脑没连网时 `a` 是空的、只有 `bt` 也出码。旧版手机认不了只有 `bt` 的码（要求 `a` 非空），有 `a` 时照旧只走局域网。配对码 `c` 兼当蓝牙口令。
  - 电脑 `pc/core/src/btpair.rs`：二维码开着（`lan.rs` `new_code` → `btpair::open`）时 `BluetoothRegisterForAuthenticationEx(NULL)` 接管所有配对确认，数字比较 / 通知配对码直接 `BluetoothSendAuthenticationResponseEx` 同意（这个调用会一直等到配对结束才返回，日志时间比手机上的完成晚一点），PIN 码方式拒绝；同意过的地址记进 `pending`。关二维码 `BluetoothUnregisterAuthentication`，口令留到 10 分钟过期。蓝牙 hello 带 `btc`、或者已连着时单独发 `{"t":"btpair","c":…}`：口令对 → `lan::bt_paired` 写 `justPaired` + `justPairedVia = "bt"`，界面关二维码、提示「已用蓝牙配对」；`pending` 里的地址拿不出对的口令 → `BluetoothRemoveDevice` 删掉配对、断开；`sweep()`（每 5 秒）删掉同意后 120 秒还没带口令连上来的。对端地址用 `RemoteHostName().RawName()`（「(78:d8:…)」）。
  - 手机 `BtPair.kt`：`getRemoteDevice(bt).createBond()`，听 `ACTION_BOND_STATE_CHANGED`，60 秒超时；已配对直接算成。成了：`LinkService.setBtPairCode`（存 `bt_pair_code` = {addr, code, at}，蓝牙正连着这台就马上发 `btpair`，否则下次 `greet` 的 hello 带 `btc`，发完就删）→ `chooseComputer` → `startLinkIfReady(address)`（教程开着时 `chooseComputer` 不连，这里必须连上去交口令，不然电脑 120 秒后删配对）→ `retryNow`。没权限 / 蓝牙没开时先要（`pendingBtQr`，`REQ_BLUETOOTH` / `REQ_ENABLE_BT` 回来接着配）。
  - `Setup.ScanFor`：`BT`（「选择电脑」里「扫码配对新电脑」）只配蓝牙；`LAN`（教程扫码页、「扫码走局域网」）只绑局域网；`ANY`（菜单「扫码连接电脑」、别的 App 打开的链接）局域网能绑就绑，蓝牙在已有权限、蓝牙开着（或者码里没有局域网地址）时顺便配，这时局域网失败不报错（`pairWith(quietFail)`）。
  - **坑**：`connectLoop` 原来对选好的电脑一直连 RFCOMM，电脑没配对时系统会自己发起一次「临时配对」（logcat `Auto-accept temporary pairing`），和 `createBond` 抢，手机报「PIN 码或通行密钥不正确」、电脑回调等 30 秒超时（错误 258）。现在没配对（`bondState != BONDED`）就不连，显示等待。
  - 测试：`%TEMP%\sn-lan\btpairtest.py bt|lan|any`（写 lan-command 开二维码、拼和界面一样的内容、`debugPairFile` + `debugPairFor`）。从头测要先删两边配对：电脑 `BluetoothRemoveDevice`（PowerShell Add-Type），手机跟着就没了。实测：已配对重扫 3.1 秒；删配对后从头扫，手机点「配对」，7.1 秒连上并核对口令，电脑没弹 Windows 的确认。adb 装完第一次启动，澎湃 OS 会把运行时权限清掉，要启动之后再 `pm grant`。
  - 没测到：口令不对时删配对、120 秒超时删配对的真机情况；只有 `bt` 没有 `a` 的码；`ANY` 两种一起。
- 电脑端 0.7.17 / 手机端 0.9.15：教程先选连接方式、App 里直接扫码、首页检查认局域网。
  - 手机 `Tutorial.kt`：`allPages` 是搭好的全部页，`pages` 是按 `mode`（`Mode.BT` / `Mode.LAN` / null）排出来的要走的页（`route()`）：蓝牙路有配对、选电脑两页，局域网路只有扫码页，没选按蓝牙算。翻页、圆点、`last`、`reachedPad` 都按 `pages`；没走的页 `layoutPages` 里藏起来。选择页没选（`blocked()`）：「下一步」变灰写「先选一种」（`NavBar.dimTo`），`settle` / `drag` / `release` 都不往后翻。第一次打开不预选；从菜单打开按现在的连接预选（局域网绑定了、而且没选蓝牙电脑或局域网正连着 → LAN）。`ChoiceScene` 是选择页的小画，`OptionCard` / `ModeIcon` / `Radio` 是两张选项卡。扫码页 `batteryHolder`：`BackgroundGuide.worthOpening` 时显示「让局域网在后台也连着」，按钮走 `Host.openBatterySettings()`。
  - 电脑 `app.slint` `TutorialDialog`：页号固定 0–10（0 欢迎、1 装好、2 选方式、3 配对、4 选电脑、5 扫码、6 布局、7 映射、8 怎么用、9 托盘、10 准备好了），`way`（0 没选、1 蓝牙、2 局域网）决定跳过哪几页，两个函数在「页」和「这条路上第几步」之间换算；`main.rs` 的 `TUTORIAL_TRAY_PAGE = 9`、`TUTORIAL_LAST_PAGE = 10`。`--tutorial 3`/`4` 自动走蓝牙、`5` 走局域网。首页 `HomePage` 多了 `lan-phones`、`lan-pair`，检查卡 `LanCheck`：绑定过手机时局域网排第一，蓝牙两项不再报警。
  - 扫码改回 App 里扫（相机权限和刷新率的复测见下面「相机权限和刷新率」那条）：扫码界面里是 60 Hz（相机预览），回到蒜鸟马上是 120 Hz。
- 手机端 0.9.14：只连局域网时切出去再回来断了、连不上。原因是澎湃 OS 默认「省电策略 → 智能限制后台运行」在息屏或切到后台约 1 分钟后冻结蒜鸟（`dumpsys activity` 里 delayFreeze / "Controlled"），连前台服务也照冻，冻结时内核把它的 TCP 断掉（手机 `Software caused connection abort`，电脑 10054）；蓝牙不受影响。改成「无限制」就不冻（同时进 deviceidle 白名单、`MILLET_NO_RESTRICT_APP`）。处理：`LinkService.wake()`（回到前台、亮屏时发一个 ping，2 秒没回就关掉重连，退避清零）；心跳发现两次之间隔了 3 秒以上（`THAW_GAP_MS`，刚解冻）也 `wake()`；扫码绑定成功后、菜单「后台保持局域网」带去放开后台限制（`BackgroundGuide`）。局域网的连接、断开写进 `Android/data/dev.suanniao/files/lan.log`（256 KB 轮换）。

- 电脑端 0.7.14 / 手机端 0.9.12：手机文件发到电脑。
  - 入口：`ShareActivity`（系统分享 `SEND` / `SEND_MULTIPLE`，`*/*`，看不见的透明界面）、菜单「发文件到电脑」（`ACTION_OPEN_DOCUMENT` 多选）。分享来的读权限只在接收的界面开着时有效，所以先 `openInputStream` 打开，再把流交给 `FileSender`（连接服务里的发送线程）慢慢读；只分享文字时发 `clip`。
  - 协议（`pc/core/src/filerecv.rs` 顶上有完整说明）：`fileStart`（id、batch、index、count、name、size、mime）→ 电脑回 `fileAck got=0` → `filePart`（Base64）… → `fileEnd` → `fileDone`（ok、name / error）；`fileCancel` 取消。电脑 hello 带 `files: true`，没有就提示先更新电脑端。
  - 流量控制：每块写好电脑回 `fileAck got`，手机发出去的最多比 got 领先一个窗口（局域网 4×64 KB，蓝牙 3×4 KB），连接里不会堆一大截文件数据，触控板、按键插得进去。最近 400 ms 有输入就先停（两种连接都是）。
  - 速度：局域网实测 2.9～4.3 MB/s（窗口 2 块约 3 MB/s、4 块约 4 MB/s、8 块约 4.5 MB/s，取 4 块：堵在触控板消息前面的最多约 60 ms）。电脑端不是瓶颈（本机假手机发，0.17 秒收 3.8 MB）。
  - 澎湃 OS 把**前台 App** 的上传排进 Wi-Fi 的视频优先级（`tc -s qdisc show dev wlan0` 看计数：前台时走 tx-5/tx-7，后台走 tx-8～11），这个网络里大块数据又慢又丢包：约 0.2 MB/s、三成的包重发；蒜鸟在后台（从相册分享）就是 3～4 MB/s。按 App 身份分的，App 改不了：试过 TOS（0x20、0xB8）、连上以后设 `SO_PRIORITY` 0、不拿 Wi-Fi 锁，都还在这条队列。和省电策略无关（白名单里照样慢）。对比方法：`adb shell "run-as dev.suanniao sh -c 'nc -q 1 电脑IP 端口 < 文件'"` 和不带 run-as 的同一条，电脑上用 Python 收着计时；蒜鸟在前台、按 Home 以后各测一次。
  - 所以按实测提示（各家手机做法不一样，不按牌子猜）：一批里局域网上 1 MB 以上的文件平均不到 1 MB/s（不算给触控板让路的时间），界面开着就弹一次（一天最多一次）：大多数块发的时候蒜鸟在前台（`ActivityManager.getMyMemoryState`），建议从相册、文件管理分享；在后台也慢、后台限制没放开，带去设置；别的（网络本身慢）不说。
  - 后台限制的引导按牌子来（`BackgroundGuide.kt`）：标准的两项（电池优化白名单、`isBackgroundRestricted`）读得到；华为、荣耀、OPPO、vivo、魅族自己的一层读不到，菜单里一直给「后台保持局域网」入口。没放开白名单时弹系统的「允许」框，有自己一层的牌子在它后面垫着自家的设置页（`startActivities`），打不开就去应用详情。说法按牌子：小米「省电策略 → 无限制」、华为荣耀「应用启动管理」、OPPO「耗电管理」、vivo「后台耗电管理」、三星和别的「电池 → 不受限制」。只在小米上实测过，别的牌子的设置页路径按公开资料写的。
  - 不点手机界面测发送：把文件 `adb push` 到 `/sdcard/Android/data/dev.suanniao/files/`，再 `adb shell am start -n dev.suanniao/.ShareActivity -a android.intent.action.SEND -t application/octet-stream --eu android.intent.extra.STREAM file:///sdcard/Android/data/dev.suanniao/files/文件名`，看 logcat 的 `SuanNIAO-file` 和后台日志的 📥。一个文件全程走开始时那条连接，断了算失败（电脑在会话结束时删掉 `.蒜鸟接收中` 临时文件）。
  - 只连蓝牙：超过 1 MB 的照片用 `ImageDecoder`（认 EXIF 方向、HEIC）缩到长边 2048、JPEG 82；其他文件超过 3 MB 不发。
  - 电脑：存到 `settings.receive.folder`（空 = `SHGetKnownFolderPath(FOLDERID_Downloads)\蒜鸟`；这台电脑的「下载」在 `E:\Downloads`），先写 `名字.蒜鸟接收中`，大小对上再改名，重名加「 (n)」；文件名去掉路径、保留名和非法字符。剩余空间少于文件 + 200 MB 拒收。
  - 一批收完（`batch` 里 count 个都有结果）：`settings.receive.clipboard` 开着时，只有一张 jpg/png/bmp/webp 就解码（按 EXIF 转正，超过 4000 万像素放文件）放 32 位 `CF_DIB`（原图是 PNG 再放 `PNG` 格式），否则放 `CF_HDROP` + `Preferred DropEffect = 复制`；写完用 `clipsync::mark_own` 记下序号，不会被剪贴板同步发回手机。托盘气泡点了 `explorer /select`。
  - 电脑设置页「接收文件」卡片：保存到（打开 / 更改…，`IFileOpenDialog` + `FOS_PICKFOLDERS`，选回默认位置就记成空）、收到后放进剪贴板。

- 电脑端 0.7.13：设置行说明文字重叠 + 全界面说明精简。
  - 原因：Slint 1.18 里，ScrollView 内折行（`wrap: word-wrap`）文字的高度在窗口改宽度后不会跟着重算（先窄后宽、内容超出要滚动时能复现），`SettingRow` 的说明用的是折行的 `Muted`，行高按旧宽度算，文字盖到下一行。
  - 改法：`SettingRow` 的说明改成单行 `Text` + `overflow: elide`，不再依赖折行测高；说明都压到最窄窗口（1040 px）下一行放得下。别的地方的折行文字（`Muted`、`WrapText`）同样有这个风险，尽量写短。
  - 截图测试：`%TEMP%\sn-text\flow.ps1` 多了 `-Size "w,h"` 和步骤 `rW,H`（运行中改窗口大小）；复现用 `-Size "1500,1500" -Steps "w2000;r1900,1500;w1500;s"`。

- 电脑端 0.7.12 / 手机端 0.9.11：动画顺滑 + 局域网审查出来的问题。
  - **相机权限和刷新率**：0.9.11 时实测声明了 `CAMERA` 就只有 60 Hz（当时以为澎湃 OS 把有相机权限的 App 归到「跟随应用内设置」），于是去掉了权限、扫码交给小米「扫一扫」。2026-10-30（0.9.15）复测推翻了：声明 + 授权 + 用 Camera2 真打开后摄，省电策略「智能限制」「无限制」下都是 120 Hz（`dumpsys display` 的 renderFrameRate、DisplayModeDirector 最新一条 `PRIORITY_MIUI_REFRESH_RATE` 最高 120）。当时的 60 Hz 多半是别的原因（那次同时在调动画、重装），没再复现。所以又改回 App 里直接扫：`scanComputer()` 没相机权限时先弹「扫码要用相机」说明（可「从相册选」），再 `IntentIntegrator(...).initiateScan()`（zxing-android-embedded 的 CaptureActivity，清单里改成 `fullSensor` 不强制横屏），结果在 `onActivityResult` 里 `parseActivityResult`。手机自带相机、「扫一扫」扫到的 `suanniao://pair?…` 照样由 VIEW intent-filter 接住（`handleLinkIntent`）：5 分钟内从蒜鸟发起的扫码直接绑定，别的来源先弹「绑定这台电脑？」；相册选图用 zxing core 认（`decodeQrImage`）。查刷新率：`dumpsys display` 里的 renderFrameRate 和 DisplayModeDirector 的 mVotes、`dumpsys activity service com.miui.powerkeeper` 的 `setScreenEffect pkg=… fps=`；复测用的临时包（加了 `--ei debugCamera 秒数` 打开后摄）在 `%TEMP%\sn-camtest`。
  - 测动画：`dumpsys gfxinfo dev.suanniao reset` → 操作 → `dumpsys gfxinfo dev.suanniao framestats`，逐帧看 `IntendedVsync` 间隔（> 1.5 倍刷新周期就是丢了帧）、主线程晚开工多久（`HandleInputStart - IntendedVsync`）、排版 / 录制 / 出图各几毫秒。这些是在 60 Hz（每帧 16.7 ms）下调的；现在是 120 Hz（8.3 ms），见上一条。
  - 面板、对话框、教程（`Overlays.Modal.attach`）：进场分两步。第一帧放在起点（屏幕外、透明），搭建、排版、绘制、预先画好硬件图层都在这一帧做完，下一帧（`Choreographer.postFrameCallback`）才开始动画。进场没走完时 `refresh()` 不原地重建，等走完再建。
  - `Overlays.Sheet` 刷新不再 `removeAllViews` 重搭：`place()` 按「种类 + 标题 + 第几次出现」找旧的行，就地改小字、能不能点、开关状态和点了做什么（`RowParts` 挂在行的 tag 上）；只有增删的行才新建。行数变化用 `LayoutTransition`（进场后才开）。菜单的 body 关掉后留着（`overlays.sheet(menuBody)`），下次打开只更新：搭建 45~80 ms → 5~17 ms，点开关时主线程卡 80~100 ms 两次 → 没有超时帧。
  - 横幅（`expandBanner`）：以前每帧改横幅高度，整张网格每帧重排、重画，一帧 18~120 ms。现在只排一次版，网格用 `translationY` + `scaleY`（pivot 在顶上）从旧位置、大小动到新的；收起时先用变换动到终点，最后一帧才改布局。收起 p50 18 → 8 ms，p90 26 → 14 ms。
  - 局域网审查（一个 Agent 读代码找的问题，逐条修了）：
    - 断线检测：手机每条连接记 `awaitSince`（发了心跳还没收到任何东西的起点），亮屏时局域网 4.5 秒没回音就关掉。电脑 `pc/lan` 认证后开 TCP keepalive（5 秒 / 1 秒），`write_all` 5 秒写不完就当连接坏了、整个断开（读的一方也会醒）；会话看门狗亮屏时局域网 5 秒没消息就断开。实测：手机关 Wi-Fi → 电脑 5 秒断开、蓝牙接着用；电脑后台挂起 7 秒 → 手机 4.5 秒断开局域网、恢复后 1 秒重连。
    - 回复走原连接：hello、config、lan、pong、mem、layoutSaved 从收到请求的那条连接发回（`link::reply`）；其他消息 `send_to_phone` 按局域网、蓝牙的顺序试，局域网写失败就关掉它再试下一条。延迟统计因此不再掺另一条连接的样本。
    - 直通按住的键：电脑的 `HOLDS` 改成所有会话共用（按代数判断作废的记录），一条断开时只有一条都不剩了才取消动作、松开所有键；看门狗看所有连接最后收到消息的时刻。手机 `route()`：还按着键、鼠标键时消息固定走按下时那条连接。
    - 多部手机：`Link.phone`（局域网认证得到；蓝牙看 hello 里新加的 `lan` 字段，手机扫码绑定时拿到的编号）。别的、仍在绑定列表里的手机连着时，新连接回 `{"t":"busy"}` 后断开，手机 30 秒后再试。手机端比较两条连接 hello 里的 `pc`，蓝牙连的是另一台电脑就不用它（只收心跳）。
    - 电脑 `pc/lan/src/connection.rs`：一次读到的密文超过 16 KB 明文缓冲时，剩下的先存着，明文取走了再喂（以前会报 "received plaintext buffer full" 断开）。配对请求可带 `prev{id,token}`，令牌对得上就删掉同一部手机的旧记录。codex 子代理改的，`cargo test -p suanniao-lan` 现在 35 个测试。
    - `lan-command.json` 加 `at`：界面二维码开着时每 30 秒重写一次，后台 90 秒没见到新的就不续配对码；`remove` 每次都带上所有还没删掉的手机（删过的再删是空操作），后台启动时也照删。
    - 手机 `LanBinding.update` / `updateIfSame`：读、改、写在一把锁里，连接线程只改同一次绑定（电脑、手机编号都没变）。局域网 socket 绑到 Wi-Fi / 有线的 `Network` 上（全局 VPN 下也能连）。`Setup.showPicker` 没蓝牙权限、蓝牙没开时也打开，里面有扫码那一行。
    - 电脑取地址时，带网关的「Hyper-V Virtual Ethernet Adapter」（外部虚拟交换机）不跳过。

- 电脑端 0.7.11 / 手机端 0.9.10：第二种连接方式——局域网（TCP + TLS），扫码绑定，和蓝牙并存。
  - 加密和认证在新 crate `pc/lan`（`suanniao-lan`，由 codex 子代理按说明写、自带 23 个测试：`cargo test -p suanniao-lan`）和手机 `LanSecure.kt`。电脑是 TLS 服务端，自签名 ECDSA P-256 证书（rustls + rcgen，ring 后端），私钥用 DPAPI 加密存；手机只认二维码里的证书指纹（DER 的 SHA-256，base64url）。TLS 建好后手机第一行发 `pair`（一次性配对码，10 分钟、新码顶掉旧码）或 `auth`（手机编号 + 长期令牌，电脑只存 SHA-256），电脑回 `paired` / `authOk` / `authFail{reason}`，10 秒内要认证完。文件在 `%APPDATA%\SuanNIAO\lan\`（`identity.json`、`phones.json`、`port`）；手机的令牌用 Keystore AES-GCM 加密后存在偏好 `lan_binding` 里（`LanBinding.kt`）。
  - 电脑后台 `pc/core/src/lan.rs`：绑定过手机或者二维码开着时才开端口（默认 47820，被占就让系统挑一个并记下）；`GetAdaptersAddresses` 取有线、无线网卡的 IPv4（跳过 TUN、Hyper-V、VPN 这类虚拟网卡），每 10 秒看一次，变了发 `{"t":"lan","pc","addrs","port","fp"}` 给手机（蓝牙连着也发）。主界面写 `lan-command.json`（`seq`、`pairing`、`remove`），后台每半秒看一次；状态写进 `status.json` 的 `lan`（地址、端口、指纹、配对码、`justPaired`、绑定过的手机）和 `links`（现在连着的方式）。
  - `link.rs` 的会话不再绑死蓝牙：`Conn::Bt` / `Conn::Lan`，每种最多一条（新的顶掉同一种的旧的），两种可以同时在；发给手机的走局域网，没有才走蓝牙。一条断开时照旧取消动作、松开按着的键。
  - 主界面：设置页「局域网连接」卡片（显示二维码、绑定过的手机、解除绑定），二维码弹窗 `LanDialog`（`pc/ui/src/lan.rs`，`qrcode` crate 画，内容 `suanniao://pair?v=1&pc&n&a&p&fp&c`）；侧栏连接状态后面写「局域网 / 蓝牙 / 局域网 + 蓝牙」。截图检查用 `SuanNIAO.exe --lan-qr`。
  - 手机 `LinkService`：蓝牙、局域网各一个连接循环，`active() = lanConn ?: btConn`；心跳两条都发（电脑每个会话有自己的看门狗），亮屏 / 息屏两条都发；**局域网的消息排进每条连接自己的写线程**（Android 不许主线程碰网络，心跳、回执是主线程发的——第一版就是因为这个一连上就断）。只在连着 Wi-Fi / 有线网时试局域网，连上 Wi-Fi 马上重试；局域网连着时拿 `WIFI_MODE_FULL_LOW_LATENCY` 锁。电脑说 `unknown` / `badToken`（解除了绑定）就标 `revoked`，不再连，提示重新扫码。扫码用 zxing-android-embedded（只加它、zxing core 和 androidx.core 本身，不带传递依赖；开了 `android.useAndroidX`），新权限 CAMERA、WAKE_LOCK、CHANGE_NETWORK_STATE（只用局域网、没给蓝牙权限时，「连接的设备」类前台服务靠它满足 Android 14 的要求）。调试：`--es debugPairFile 路径` 当作扫到了文件里的二维码内容。
  - 实测（小米 13 在 HFUT-WiFi 10.4.x，电脑有线 10.200.x，跨网段路由可达）：扫码到连上 1.1 秒；「测延迟」局域网 一般 8 / 慢 18 / 最慢 24 ms，只有蓝牙 20 / 47 / 67 ms；电脑上解除绑定 → 局域网断、蓝牙接着用、手机显示要重新扫码 → 重新扫码恢复；手机关蓝牙只剩局域网照样已连接，开蓝牙后两条都在。二维码截图用 ZXing 解出来和状态一致。

- 电脑端 0.7.10 / 手机端 0.9.9：左上角的「蒜鸟」换成画好的品牌字（用户逐轮看样稿定的）。
  - `tools/logo/wordmark.py` 生成：字形取思源黑体 Heavy（Noto Sans SC，OFL，`tools/logo/fonts/` 里只留了这两个字和许可证），「蒜」去掉草字头的两竖（第 10、11 个轮廓，横留着）换成两棵穿过横的蒜苗，「鸟」的点（第 3 个轮廓）换成转了 -30° 的蛋。
  - 分三层：字身（电脑 `colorize: Theme.fg`，手机 `setTint(palette.fg)`）、蒜苗（浅色 #6DB81F / 深色 #BEF264）、蛋（固定 #FFC94D，文件里就是这个色）。电脑 `pc/assets/wordmark*.svg` 用 `Wordmark` 组件（一个字 25px，原来 21px）放在主窗口和安装程序的侧栏顶上；手机 `drawable/wordmark*.xml` 叠成 `LayerDrawable`，高 26dp（一个字约 22dp，原来 18sp）。

- 电脑端 0.7.9 / 手机端 0.9.8：检查更新，安装程序换成自己做的。仓库改成了公开的（Release 不用登录就能下载）。
  - 每次发版 `release.yml` 在 Release 里多放一个 `latest.json`：两端的版本号、文件名、下载地址、大小、SHA-256，`notes` 是 `CHANGELOG.md` 这一版那段里顶格的「- 」，`pc.notes` / `android.notes` 是写在「电脑端…」「手机端…」下面的（「两端一起…」下面的两端都算）。两端读 `https://github.com/drhhhao/SuanNIAO/releases/latest/download/latest.json`。
  - 电脑：`pc/update`（WinHTTP 读 latest.json、下载、BCrypt 算 SHA-256；查到的结果和用户的选择记在 `%APPDATA%\SuanNIAO\update.json`，后台和主界面共用，读写用命名互斥量排队）。后台启动 1 分钟后查一次，之后每 6 小时（没查成 30 分钟后再试）；主界面没开着时托盘气泡提示一次（每个版本一次），托盘菜单里有「更新到 x」（打开 `SuanNIAO.exe --show-update`）。主界面打开 3 秒后查（上次超过 1 小时），查到没提示过的版本就弹更新窗口；「以后再说」一天内不再自己弹，「跳过这个版本」只是不再提醒。「立即更新」把安装包下到 `%TEMP%\SuanNIAO-update\`，大小和 SHA-256 对上才运行 `安装包 --update --dir 主界面所在的文件夹`，主界面先存好改动再退出。
  - 安装包：`pc/setup` 的启动程序 `SuanNIAO-Setup.exe` 后面接上 LZMS 压缩的两个 exe（`suanniao-pack` 打包，打完解开核对）。双击后解到 `%TEMP%\SuanNIAO-Setup-进程号\`，运行里面的 `SuanNIAO.exe --setup`，结束后删掉。安装界面是主界面程序里的 `SetupWindow`（`pc/ui/src/setup.rs`），和蒜鸟一个样子；参数 `--update`（不问、直接装，装好打开新版）、`--dir 文件夹`、`--silent`、`--no-desktop`。登记的 AppId 和 0.7.6 的 Inno Setup 安装包一样，装着 0.7.6 的直接覆盖、卸载入口换成新的（`SuanNIAO.exe --uninstall [--silent] [--remove-data]`）。
  - 装、更新：关掉那个文件夹里开着的主界面和后台 → 两个 exe 先复制成 `.new` 再改名盖过去（还被占着就把旧的改名让开）→ 开始菜单（和桌面）放「蒜鸟」→ 写卸载登记 → 后台原来开着的话再打开。卸载：关掉 → 删快捷方式（只删指着这个文件夹的）、程序、开机自启（只删指着这个文件夹的）、登记 → 勾了的话删设置 → 退出后由 cmd 删掉自己。
  - 测试安装程序：环境变量 `SUANNIAO_SETUP_SANDBOX=文件夹` 把开始菜单、桌面换到这个文件夹里，注册表换到 `HKCU\Software\SuanNIAO-SetupTest`，不打开装好的程序，窗口放在屏幕外面。测试检查更新：主界面 `SUANNIAO_UPDATE_URL=http://127.0.0.1:端口/latest.json`（这时也认 http 的下载地址）。
  - 手机：`Updater.kt`，打开 App 时查（离上次超过 6 小时），`⋯` 菜单里「检查更新」、「自动检查更新」。安装包下到 App 缓存（断了接着下），核对大小、SHA-256、包名、版本、签名后交给系统安装器（`ApkProvider`）。新加 `INTERNET`、`ACCESS_NETWORK_STATE`、`REQUEST_INSTALL_PACKAGES` 权限。调试版的签名和正式版不一样，不给下载，只给「去网页下载」。

- 电脑端 0.7.8 / 手机端 0.9.7：剪贴板同步加上图片，只从电脑到手机（手机上很多 App 复制图片不进系统剪贴板，反过来用处不大）。
  - 电脑：剪贴板里没有文字、有图片（注册格式 `PNG`，或者 `CF_DIB` / `CF_DIBV5`）时，`pc/core/src/clipimage.rs` 编成不超过 512 KB 的文件（实测蓝牙约 55 KB/s；分块 16 KB 最快，48 KB 反而慢）：现成的 PNG 够小就原样发，位图编成 PNG（32 位透明度全 0 的当成不透明），放不下就 JPEG（质量 85，透明处垫白），还放不下就按面积缩小。同一张（按原始内容的指纹）不重发。
  - 分块发：`clipImg`（id、mime、size、w、h）→ 每块 16 KB 原始字节的 `clipImgPart`（Base64）→ `clipImgEnd`；块小，传的时候心跳回复、配置推送也插得进去。传的途中剪贴板又变了发 `clipImgCancel`。日志里记编码耗时、传输秒数和速度。
  - 手机：收进缓存的 `clip/` 文件夹，收齐、大小对上了才把 `content://dev.suanniao.clip/文件名` 放进剪贴板（标签和文字一样是「蒜鸟·电脑剪贴板」，不会发回电脑）。`ClipProvider` 只读、不导出，粘贴的 App 靠剪贴板的临时授权读；留最新两张，更早的删掉。蒜鸟在前台时顶上提示接收中 / 已放好。

- 电脑端 0.7.7 / 手机端 0.9.6：手机和电脑同步剪贴板里的文字（设置里「和手机同步剪贴板」，`settings.clipboardSync`，默认开，推给手机的配置里是 `clipSync`）。消息都是 `{"t":"clip","s":文字}`，最多 256 KB。
  - 手机 → 电脑：安卓只让前台、拿着焦点的 App 读剪贴板，所以手机在蒜鸟拿到焦点（`onWindowFocusChanged`）、收到配置（刚连上）、在蒜鸟里复制时看一眼剪贴板描述（不读内容，系统不弹「已读取剪贴板」）：时间戳和上次发过的（`clip_sent_stamp`）不一样、是文字、没标敏感、不是电脑发来的，才读出来发过去（`ClipSync.kt`）。电脑放进剪贴板（不带标记，进 Win+V 历史）以后再按粘贴键，和这条按键消息走同一条连接、排在后面，所以粘的就是手机上复制的。
  - 电脑 → 手机：后台用只收消息的窗口登记 `AddClipboardFormatListener`（`pc/core/src/clipsync.rs`），剪贴板变了、是文字就发给手机，手机放进剪贴板（标签「蒜鸟·电脑剪贴板」，回到前台时认出来不再发回去）。带 `ExcludeClipboardContentFromMonitorProcessing`、`CanIncludeInClipboardHistory = 0`、`CanUploadToCloudClipboard = 0` 的不发：密码管理器复制的密码，和语音输入借剪贴板时写的字。手机发来的写进剪贴板后记下序号，那次变化不发回去；和上一次同步的一样也不发。

- 电脑端 0.7.6 / 手机端 0.9.5：发版改成两端各一个安装包，下载下来装上就能用。
  - 电脑端：`SuanNIAO-PC-版本-Setup.exe`（Inno Setup 做的，脚本 `pc/installer/SuanNIAO.iss`）。只给当前用户装，装在 `%LOCALAPPDATA%\Programs\SuanNIAO`，不用管理员权限；开始菜单里有「蒜鸟」，桌面快捷方式可选，装完可以直接打开。更新就是装新版的安装包，设置都留着。
  - 安装界面是中文的，只有两步：选要不要桌面图标、点「安装」，然后是「安装完成」（勾着「打开蒜鸟」）。右上角的小图和完成页左边的竖图是蒜鸟。
  - 装新版、卸载之前，安装程序先运行装好的 `SuanNIAO.exe --quit`：关掉同一个文件夹里的程序开着的主界面和后台，别处的副本（比如 `pc\target\release` 里的）不碰。
  - 卸载（「设置 → 应用」里）：删掉指向安装目录的开机自启，再问要不要把设置（`%APPDATA%\SuanNIAO`）也删掉，默认不删；静默卸载不问、不删。
  - 手机端：正式版安装包里带着「快」那档语音模型（x-asr-480ms），装好就能用，不用再推模型；安装包约 160 MB。模型直接从安装包里读（sherpa-onnx 从 assets 读），不用先解压出来，加载时间和放在手机存储里一样。「更准」那档（960ms）还是用 adb 推。
  - 手机「关于」里加了「开源许可」：sherpa-onnx、X-ASR、ONNX Runtime、Nunito 字体的说明和许可证全文。

- 电脑端 0.7.5 / 手机端 0.9.4：内部名字改成 SuanNIAO。
  - 电脑端：主界面 `SuanNIAO.exe`、后台 `SuanNIAO-core.exe`，数据文件夹 `%APPDATA%\SuanNIAO`；单实例的互斥量、托盘窗口类、开机自启（注册表 Run 里的 `SuanNIAO`）、排查用的环境变量（`SUANNIAO_TRACE`、`SUANNIAO_DRYRUN`）也换了名字。
  - 从旧版更新：第一次打开新版主界面时，关掉还开着的旧主界面（PhonePad.exe），让旧后台（phonepad-core.exe）退出，把 `%APPDATA%\PhonePad` 整个改名成 `%APPDATA%\SuanNIAO`，开着的开机自启换成指向新后台。新后台先启动时（比如开机自启）也会搬数据文件夹。旧后台没退出、文件夹改不了名时复制一份，旧文件夹留着。
  - 手机端：包名从 `dev.phonepad` 改成 `dev.suanniao`。系统把它当成另一个 App，不能覆盖旧版：要另外装、重新给权限、重新选电脑；旧 App 的语音模型在 `/sdcard/Android/data/dev.phonepad/files/models`，可以用 `adb shell cp -r` 复制到新包名的目录下（见「本地语音识别」）。两个都是调试版时，设置（选好的电脑、布局、语音设置）也能带过去：新 App 不能开着（开着就先 `adb shell am force-stop dev.suanniao`，不然它内存里的空设置会盖掉复制过去的），再 `adb shell "run-as dev.phonepad cat shared_prefs/phonepad.xml | run-as dev.suanniao sh -c 'mkdir -p shared_prefs && cat > shared_prefs/suanniao.xml'"`；权限可以用 `adb shell pm grant dev.suanniao android.permission.BLUETOOTH_CONNECT` 直接给（`RECORD_AUDIO`、`POST_NOTIFICATIONS` 同样）。
- 电脑端 0.7.4：更新以后再打开蒜鸟，开着的旧版窗口会自己关掉、换成新版。以前只会把旧窗口调出来，要先手动关掉旧窗口才用得上新版。
  - 每个主界面在自己的窗口上挂着版本号，属性名和单实例的互斥量同名，测试副本换掉这个名字就碰不到正在用的窗口。
  - 0.7.3 以前的窗口没挂版本号，按标题「蒜鸟」、窗口类和程序所在的文件夹认。
  - 开着的不比自己旧（同版或更新）时，照旧只把它调出来。
- 电脑端 0.7.3 / 手机端 0.9.3：加了使用教程，两边都是第一次打开时自动出来，从「欢迎使用蒜鸟」开始一页页讲，每页一幅会动的小画加几句大白话。
  - 手机上 9 页：在电脑上打开蒜鸟 → 蓝牙配对 → 选电脑（这三步就在教程里做）→ 触控板 → 按键 → 语音 → 「⋯」菜单 → 准备好了。之后在「⋯ → 使用教程」里能再看。
  - 电脑上 9 页：装好手机端、配对、选电脑（页面上实时显示后台、蓝牙、配对、连接的状态）→ 布局 → 映射 → 手机上怎么用 → 关掉窗口照样工作（顺带开机自启开关）→ 准备好了。之后点首页右上角的「使用教程」能再看。
  - 电脑上是否看过存在 `%APPDATA%\SuanNIAO\ui.json` 的 `tutorialSeen`；看到开机自启那页就记下 `autostartAsked`，首页不再重复问。已经回答过开机自启、或者开着开机自启的老用户，不会自动弹出。
  - 调试：电脑端 `SuanNIAO.exe --tutorial N`（从第 N 页打开，0 起）、`--first-run`；手机端 `am start -n dev.suanniao/.MainActivity --ei debugTutorial N`。
- 电脑端 0.7.2 / 手机端 0.9.2：触控板移动顺滑了，轻点一下再接着移动也一样。蓝牙送消息时快时慢（常常三四十毫秒一条都不来，接着几条一起涌进来），以前照收到的节奏走指针，一顿一跳；现在手机在每条移动里带上手指移动的时间，电脑按这个时间匀速回放（`pc/core/src/playout.rs`）。代价是跟手慢一点：延迟中位数比以前多约 25 毫秒，但不再忽快忽慢。点击、按下、松开和移动排同一个发送队列，按先后顺序到电脑，不会点在半路上。
- 电脑端 0.7.0 / 手机端 0.9.0：改名叫「蒜鸟」。exe 里带了版本信息，任务管理器里显示「蒜鸟」「蒜鸟后台」；复制动作的开头改成 `suanniao/1`、导出备份的格式名改成 `suanniao-backup`，改名前的 `phonepad/1`、`phonepad-backup`、`phonepad-config` 照样认。
- 手机端 0.8.1：「说完一句再发」（不边说边打字）时，停顿判定按模型出字的间隔加长（960ms 模型 1.44 秒，480ms 还是 0.8 秒），不再把没说完的话一段段发出去。

- **电脑端**：主界面 `SuanNIAO.exe`（窗口标题、任务管理器里叫「蒜鸟」）加后台 `SuanNIAO-core.exe`（任务管理器里叫「蒜鸟后台」）。
  - 后台在托盘常驻：左键托盘图标打开主界面，右键菜单可以「暂停按键 / 恢复按键」、退出。托盘提示写「蒜鸟 · 已连接 · 手机名」或「蒜鸟 · 未连接」。
  - 主界面有六页：首页、布局、映射、总览、设置、关于。改动会自动保存，手机马上更新。
  - 顶栏：连接状态（手机名太长时省略，点一下弹出手机信息：名字、型号、App 版本、电量、连上多久）、「按键映射」总开关（关掉后手机上的按键不执行、变淡，触控板和语音照常；手机上也能暂停 / 恢复）、撤销 / 重做。
  - 首页：
    - 没连上时显示连接自查：后台在不在运行、电脑蓝牙开没开、和手机配对过没有，每一项不对都写了怎么办；「连不上？」展开更多排查办法。
    - 连上以后显示同步状态：「已同步」「等手机确认…」，手机 5 秒没确认时写「手机没有确认（可能是旧版 App）」。
    - 第一次打开时问一次「开机时自动启动后台」，回答过就不再问（记在 `ui.json`）。
    - 今天的按键次数、语音字数、最近动作。
  - 主界面启动时发现后台比自己旧（刚更新完、旧后台还开着），会让旧后台退出、换成新的。
  - 已经开着一个主界面时：开着的比自己旧，就让它关掉（和点 × 一样），自己接着打开；不比自己旧，就只把那个窗口调出来，不碰后台。
  - **动作**：按键、组合键、鼠标按键（左 / 右 / 中 / 侧键后退 / 前进）、鼠标滚轮、光标移动（1～500 像素）、输入文本（跟随设置 / 逐字 / 剪贴板）、等待（0～300000 毫秒，只推迟后面的步骤）、打开应用（参数；「已经在运行就切到它的窗口」）、打开网站（只认 http、https，不写协议补 https://）、媒体控制、切换方案、禁用。
    - 每一步可以单独停用；按键下拉框分组（常用、修饰键、标点、媒体、字母数字、F1～F24），能搜索。新加的一步先是空的，标红「请选择按键」，没选的步骤执行时跳过。
    - 看不懂的步骤（更新的版本或别的软件加的）原样保留、执行时跳过，界面上显示成只读的一行。一步看不懂不会连累整份配置。
    - 动作在后台的单独线程里排队执行：「等待」不卡别的按键；改配置、断线、暂停时没执行的步骤取消。
    - 打开的程序、网页会调到前台（后台进程直接打开的默认抢不到前台，只在任务栏闪）。
  - **剪贴板方式输入文本**（语音文字、「输入文本」动作）：先把剪贴板里原来的全部内容存下来（文字、HTML、RTF、图片的 DIB / DIBV5 / PNG、复制的文件、各程序自己的格式、增强图元文件），放进文字按 Ctrl+V，1 秒后原样放回。
    - 临时写进去的文字和放回去的内容都带「不进剪贴板历史（Win+V）、不上传云剪贴板」的标记，剪贴板历史里不会多出东西。
    - 粘贴以后用户自己又复制了东西（剪贴板序号变了），就不放回，免得盖掉新内容；连着粘贴几次只在最后放回一次。
    - 剪贴板里有存不下来的东西（程序私有格式、超过 256 MB）时这次改成逐字输入，剪贴板一点不动。
  - **方案**：布局所有方案共用，每套方案只存每个按键的动作和「按住时」。映射页顶部的下拉框切换，旁边「新增」（真正空的方案）、「复制」（带着现在的映射）、重命名、删除（至少留一个）。首页、总览、手机上显示的都是当前方案。
    - 配置文件里是 `profiles` + `activeProfileId`；当前方案的映射同时照旧写在模块上，旧版读新配置照样能用。没有方案的旧配置读进来自动变成「默认方案」；旧名「默认配置」读到时改名。
    - 「切换方案」动作（上一个 / 下一个，到头后回绕或停住）：放进某个按键时先确认，然后所有方案里这个键的同一种按法都换成它（保证切过去还能切回来）；删掉时其他方案里的一起删（也先确认）。切换后电脑桌面弹通知（置顶、不抢焦点、鼠标能穿过去，在鼠标所在显示器上水平居中、竖直 75%），2 秒后淡出；手机顶栏闪一下新方案名。
  - **映射页**：左边的按键列表按页分组（可以收起）、能搜索按键名或动作；右边是「单击 / 双击 / 长按」三种按法和「按住时」。
    - 「按住时」四选一：长按一次、重复长按、连发、直通（说明写在副标题里）。连发、直通时双击和长按不会生效：这两个按法变灰，配了动作的会提示怎么改。
    - 换一个按键时按法回到「单击」，标题写成「↑ · 长按」这样。
  - **撤销 / 重做**：布局、映射、设置的改动都能撤销，最多 100 步；Ctrl+Z、Ctrl+Shift+Z、Ctrl+Y，顶栏也有按钮。提示条写「已撤销：做了什么」。连着打字、加减同一处的合成一步。文件被别处改了（手机上调布局、托盘暂停、手机上切方案）时，同样的改动合进撤销记录；和别处改了同一个地方的步骤丢掉，不会撤销到别人的改动上。
  - **复制 / 粘贴动作**：`suanniao/1` 文字格式（也认改名前的 `phonepad/1` 和旧版的格式头），能从引用块、代码块里认出来，格式不对时提示第几行。
  - **一键预设**：常用、写代码和终端（Shift+Enter、Ctrl+C、Ctrl+L、Ctrl+Z……）、导航和鼠标、媒体、清空（删除全部文本 = Ctrl+A 再 Backspace，放在最后）。选了替换当前按法，可以撤销。
  - **已保存的动作序列**：把当前按法起名保存（存在配置的 `savedSequences`），以后插入或替换到别的按法，可以改名、删除。
  - **录制**：按键、组合键旁边点「录制」，按一个键就录这个键，按组合键就改成组合键；Esc 取消。分不出左右修饰键（都录成左边的），要右边的在下拉框里选。
  - **布局页**：
    - 模块的位置、大小以格为单位，可以是小数。选中的模块四条边、四个角都能拖，拖中间移动；默认对齐 1/4 格，按住 Alt 不对齐；最小 0.5 格，不能出界。属性面板里也能直接输入数值。
    - 拖到一样大的模块上（或者被挤开的模块放得进原来的位置）就互换位置。画布按窗口高度缩放。
    - 这一页放满了时「添加按键」放到新的一页，提示「这一页满了，放到了新页面「第 2 页」」，能撤销。
    - 删除页面、模块前先确认并写清影响（几个按键、几个方案里的映射），删了以后提示条上能撤销。改网格前先预览、确认。
  - **设置页**：触控板、语音输入（分「语音按钮」「语音输入框」两组）、按键判定、后台程序、导出和导入几张卡片，前三张能单独「恢复默认」；单位、小数位都写清楚。
    - 「导出和导入」：导出备份…（布局、方案、设置、已保存的序列；导出后提示条上能「打开所在文件夹」）、导入备份…（先整份校验，有一处不合法就整份不导入并说明是哪一处，确认前给摘要，导入前把现在的配置备份到 `backups\`，导入后能撤销）、导入遥控器映射…（见下）、打开日志目录。
  - **导入遥控器映射…**：从遥控器映射软件导入，「从本机读取」（把它的数据复制到临时文件夹再读，只读不写）或「选择文件…」（它导出的映射文件或数据备份）。先预览勾选，确认后新建一页「遥控器」，13 个键按遥控器的样子摆好；默认导入成新方案（缺省名「遥控器映射」，重名时加序号，其余按键沿用当前方案），也可以写进当前方案。
  - **配置文件读坏了**（比如被别的程序写坏）：坏文件原样留成 `config.broken-时间.json`；主界面顶上出横幅，写明哪里坏了，可以「打开文件夹」「从最近的备份恢复」「用默认布局」。后台继续用上一份好的配置、不推默认配置给手机，status.json 里写 `configError`；后台启动时就读坏了，先用最近一份能读的备份（只读，不改配置文件）。
    - 配置读好时每天留一份自动备份 `backups\config-auto-日期.json`，最多留 7 天。
  - 文件对话框在单独的线程里开，开着时主界面不占 CPU。
  - 后台日志 `logs\core.log` 5 MB 一个，轮转成 core.1.log … core.5.log，最多留 5 个旧的。
  - 手机电量显示在首页、侧栏、顶栏；20% 以下变成提醒色，充电时写「充电中」。
  - 配置文件被别处改了（手机上调了布局）会自动重新载入；界面里还有没保存的改动时三方合并，两边都留下。
- **手机端**：按电脑下发的「网格 + 模块」布局显示。
  - 模块有四种：触控板、按键（单击 / 双击 / 长按）、语音输入框、语音按钮。
  - 按键「按住时」有四种（电脑上设，计时在手机上）：
    - 长按一次（默认）：按住到长按时间触发一次长按；
    - 重复长按：到长按时间一次，隔「长按重复延迟」（350 毫秒）再一次，之后每「长按重复间隔」（100 毫秒）一次，松手停；
    - 连发：按下立刻单击一次，按住超过「连发延迟」（默认 400 毫秒）后每「连发间隔」（默认 50 毫秒）再单击一次，松手就停；连发「光标移动」时 3 秒内加速到 (1 + 光标加速度/100) 倍；
    - 直通：单击正好是一个按键或组合键时能选，按下就按住这些键、松手抬起，像实体键盘一样（修饰键共用计数，别的键 400 毫秒后每 80 毫秒补发按下）。手机切到后台、被打断时马上松开；电脑这边断线、息屏、10 秒没消息、改配置、暂停、退出时也全部松开。
    - 没选过的按键：单击只有一个滚轮或光标移动的默认连发，其余默认长按一次。连发、直通的按键不判定双击、长按（配好的动作保留）。
  - 语音按钮不弹输入法：按住说话、松手结束；上滑锁定，锁定后 20 秒没说话自动结束。
  - 语音按钮默认用**手机本地识别**（见下一节）：全程不断麦，边说边出字、带标点，默认边说边往电脑上打字。
  - 断线时说的话存在手机上，连上后问「发送 / 复制 / 丢弃」；发送时如果没真的发出去会说明，不假装发出去了。
  - 右上角 ⋯ 菜单（自绘面板）：暂停按键、语音识别（本地模型 480ms / 960ms 或系统语音引擎，三选一）、边说边打字、选择电脑、使用教程、手势说明、关于、调试、断开连接。
  - 「调整布局」：只给选中的模块画抓手，拖边、角改大小，拖中间移动，规则和电脑端一样；调整时按键不发给电脑。点「完成」发给电脑校验后写回配置；没连上时先存在手机上，连上后自动发。
  - 返回键只把 App 退到后台，不断开。通知栏里能「暂停按键 / 恢复按键」「断开」。
  - 手机上没装本地模型时，退回小米「系统语音引擎」（`com.xiaomi.mibrain.speech`）：要联网，一句一句出、没有标点，句子之间约 1 秒听不到。
    - 它自己也要有麦克风权限，没有的话会报「权限不足」（错误 9）。
    - 遇到这种情况 App 会弹窗说明，点「去开启」直接跳到它的权限页。

## 本地语音识别

- **引擎**：[sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) 2.0.1（`android/app/libs/sherpa-onnx-2.0.1.aar`，只打包 arm64）。
- **模型**：[X-ASR-zh-en](https://github.com/Gilgamesh-J/X-ASR) 流式 zipformer，中英混说，自带标点和大小写，Apache-2.0。
  - 用的是 sherpa-onnx 转好的 int8 版，每个约 169 MB，有两档：
    - `x-asr-480ms`：默认，出字快（约每 0.5 秒一段）；
    - `x-asr-960ms`：出字慢一倍，公开测试集上错字少一点。
  - 来源：`https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05.tar.bz2`（960ms 把 480 换成 960）。
- **做法**：一次按住从头到尾只用一个识别流、中途不重置。
  - 句子之间的「。」要等听到下一句开头才出，重置会丢标点，正好在重置时说的字也会被切坏。
  - 这样识别出的文字只会往后追加，所以可以边说边打：手机只发新增的字（`text`，`mode: "type"`，中间几段 `enter: false`）。
  - 松手后补 1 秒静音收尾，最后一句没有标点就补「。」。
- **装模型**：正式版安装包里带着 `x-asr-480ms`（放在 assets 的 `models/` 下，四个文件约 160 MB；发版时 `release.yml` 从上面的地址下载、核对指纹，`android/app/build.gradle.kts` 把它打进去）。本机打的 debug 包默认不带，装得快；要带就加 `-PbundleModel`（取自 `models\x-asr-480ms`）。
  其他模型（比如 960ms）用 adb 推到 App 的专属目录（重装 App 不会丢，卸载会丢），和安装包里的同名时用安装包里的。电脑上的模型在 `models\` 下，目录名就是菜单里显示的名字：

```
adb push models\x-asr-960ms /sdcard/Android/data/dev.suanniao/files/models/
adb shell chmod -R 777 /sdcard/Android/data/dev.suanniao/files/models
```

  在 Git Bash 里执行要先 `export MSYS_NO_PATHCONV=1`，不然手机上的路径会被改写。adb 建的目录默认 App 进不去，所以要 chmod。

  手机上装过改名前的 App（`dev.phonepad`）的，模型不用再推：先打开一次新 App（建好它的目录），再 `adb shell cp -r /sdcard/Android/data/dev.phonepad/files/models /sdcard/Android/data/dev.suanniao/files/`，照上面 chmod。

- **测试**（不用出声）：`tools\` 下有电脑端的流式模拟和一个接字的测试窗口，`models\test\` 下是两段 Windows 语音合成的测试音频。
  - `python -X utf8 tools\asr_stream_test.py models\x-asr-480ms models\test\vibe.wav 0`：在电脑上模拟，最后一个参数 0 表示单流不断句。
  - 手机上把 wav 当成对着语音按钮说话（只在调试版里有）：先把 wav 推到 `/sdcard/Android/data/dev.suanniao/files/test/`，再
    `adb shell am start -n dev.suanniao/.MainActivity -f 0x20000000 --es debugWav /sdcard/Android/data/dev.suanniao/files/test/vibe.wav`（加 `--ez debugLatch true` 按锁定的方式说）
  - 调试版的其他测试入口（同样用 `am start` 传）：`--ez debugOnboarding true`（第一次打开时的教程）、`--ei debugTutorial 页码`（从第几页打开教程，0 起）、`--ez debugGuide true`、`--ez debugMenu true`、`--ez debugPending true`、`--el debugDimMs 毫秒`、`--es debugConfigFile 文件`。
  - 检查更新：`--es debugUpdateUrl http://电脑的地址/latest.json`（空字符串换回 GitHub，马上查一次），加 `--ez debugUpdateAuto true` 当成打开 App 时的自动检查，加 `--ez debugUpdateInstall true` 让调试版也下载、安装（装的得是同一台电脑打的、versionCode 更大的调试版）。
  - 往电脑上打字的测试之前，先用 `tools\type_target.ps1` 开测试窗口，并用 `tools\foreground.ps1` 确认它在前台，别打进别的窗口。

## 目录

| 路径 | 内容 |
|---|---|
| `pc/config` | 电脑端共用的配置：布局、映射、方案、设置；动作序列文字格式、预设、导出导入、从遥控器映射软件导入（Rust） |
| `pc/core` | 后台：托盘、蓝牙串口服务、执行动作、剪贴板借用和还原、切换方案的桌面通知 |
| `pc/ui` | 主界面（Rust + Slint） |
| `pc/update` | 检查更新：读 latest.json、下载安装包、核对 SHA-256，`update.json` 的读写（后台和主界面共用） |
| `pc/setup` | 安装包的启动程序 `SuanNIAO-Setup.exe` 和打包工具 `suanniao-pack`；安装界面本身在 `pc/ui/src/setup.rs` |
| `pc/assets` | 图标（由 `tools/logo/make_icons.py` 生成）；`app.rc.in` + `resource.rs` 是几个 exe 共用的资源模板（图标、版本信息），由各自的 build.rs 填好编进去 |
| `android` | 手机 App（Kotlin；第三方依赖只有 sherpa-onnx） |
| `.github/workflows` | 自动发版的流水线 |
| `models` | 本地语音模型（打进手机安装包、推到手机上用）和测试音频；太大，不进 GitHub 仓库 |
| `tools` | 语音识别的测试脚本、接字的测试窗口 |
| `archive/` | 旧草稿和每次大改前的源码备份；不进 GitHub 仓库 |

配置文件和日志都在 `%APPDATA%\SuanNIAO\`：

| 文件 | 内容 |
|---|---|
| `config.json` | 当前配置 |
| `status.json` | 后台运行状态（连接、手机信息、同步回执、配置错误……） |
| `ui.json` | 主界面自己的小状态（问没问过开机自启 `autostartAsked`、看没看过使用教程 `tutorialSeen`、上次打开的版本 `lastVersion`） |
| `update.json` | 检查更新：上次查的时间和结果、自动检查开没开、跳过的版本、点「以后再说」的时间、主动提醒过的版本 |
| `logs\core.log` | 后台日志（5 MB 轮转，留 5 个旧的） |
| `backups\` | 每天的自动备份 `config-auto-日期.json`（留 7 天），导入前的备份 |
| `config.broken-时间.json` | 读坏的配置文件，原样留着 |

## 编译

电脑端需要 Rust 和 VS C++ 生成工具。两个程序会生成在 `pc\target\release\`，桌面上的「蒜鸟」快捷方式指向这里。

```
cd pc
cargo build --release
```

安装包：`cargo build --release` 会顺带编好启动程序 `SuanNIAO-Setup.exe` 和打包工具，再运行
`target\release\suanniao-pack.exe target\release\SuanNIAO-Setup.exe target\installer\SuanNIAO-PC-版本-Setup.exe target\release\SuanNIAO.exe target\release\SuanNIAO-core.exe`。

开发时可以用 `cargo build --profile fastrel`（不做 LTO，增量编译，生成在 `pc\target\fastrel\`）。单元测试：`cargo test`（后台的剪贴板测试在自己建的窗口站里跑，不会动到正在用的剪贴板）。

查触控板卡不卡：启动后台前设环境变量 `SUANNIAO_TRACE=文件路径`，后台把收到的每条消息和指针走的每一步连同时间记下来；再设 `SUANNIAO_DRYRUN=1` 就只记录、不真的动鼠标键盘（自动化测试用，免得点到正在用的窗口）。记下的文件可以拿 `playout.rs` 里的 `replay` 测试离线回放、换参数比较，用法写在那个测试上面。

手机端需要 JDK 17 和 Android SDK：

```
cd android
%LOCALAPPDATA%\Gradle\gradle-8.11.1\bin\gradle.bat assembleDebug
adb install -r app\build\outputs\apk\debug\app-debug.apk
```

## 发版

代码在 GitHub 公开仓库 `drhhhao/SuanNIAO`。推送到 `main` 以后，GitHub Actions（`.github/workflows/release.yml`）先看「电脑端版本 + 手机端版本」这一对发布过没有：

- 没发布过：在 GitHub 的 Windows 机器上编电脑端、用 `suanniao-pack` 做安装包，在 Linux 机器上下载语音模型、编手机端正式版并签名，然后发一个 Release，名字像 `pc0.7.9-android0.9.8`，附带 `SuanNIAO-PC-0.7.9-Setup.exe`、`SuanNIAO-Android-0.9.8.apk` 和两端检查更新读的 `latest.json`。发布说明取 `CHANGELOG.md` 里从这一版到上一次发布之间的几段（中间没发布过的版本一起算）。
- 发布过：什么都不做。所以发新版就是改版本号、再推送。

也可以在仓库网页的 Actions → 发版 → Run workflow 手动跑。

- 电脑端在 GitHub 上编译时把 C 运行库静态链进 exe（`+crt-static`），别人的电脑没装 VC++ 运行库也能打开。
- 手机正式版用发版密钥签名，和本机 debug 包的签名不一样，不能互相覆盖安装。
- 下载的语音模型核对 SHA-256，换版本时 `release.yml` 里的地址和指纹要一起改；手机包里没带齐模型的四个文件就不发。
- 电脑端安装包没有代码签名，Windows 第一次会提示「Windows 已保护你的电脑」，发布说明里写了怎么点过去。

## 通信（蓝牙串口或局域网 TLS，每行一条 JSON，两种连接的消息一样）

| 方向 | 消息 |
|---|---|
| 手机 → 电脑 | `hello`（`device` 手机的蓝牙名、`model` 型号、`app` App 版本）、`ping`<br>`move` / `scroll` / `mouse`（触控板；`move` 带 `ts`：这段移动最后一个触摸点的手机时间（毫秒，带小数），电脑按它匀速回放，旧版 App 不带时按到达时间摊开；`mouse` 和 `move` 走同一个发送队列，按先后顺序到）<br>`button`（按键 id + `click`/`doubleClick`/`longPress`；连发、重复长按时带 `seq`（0 = 第一下）、`ts`（手机时刻，电脑跟不上时跳过过时的几下）和 `held`（按住了多少毫秒，光标加速用））<br>`hold`（直通的按键：`id` + `down: true/false`）<br>`battery`（电量 `level` 0～100、`charging`，连上时和变化时发）<br>`text`（语音文字；边说边打时 `mode: "type"`）<br>`key`（边说边打时识别结果改了字，先退格）<br>`state`（亮屏 / 息屏）<br>`layout`（手机上调好的布局：`page` + `modules: [{id, x, y, w, h}]`）<br>`configAck`（`rev`：套用完了哪一次推送的配置）<br>`setEnabled`（`enabled`：在手机上暂停 / 恢复按键，后台写进配置的 `settings.enabled` 再推送配置） |
| 电脑 → 手机 | `hello`（`host` 电脑名、`app` 后台版本）、`pong`<br>`config`（布局（含 `snap`、`minSize`）、各按键有哪些按法（看有没有启用的步骤）、`hold`（按住时）和兼容旧版的 `repeat`、触控板和语音参数、连发和长按重复的延迟、间隔、`paused`（暂停了按键映射）、`profile`（当前方案 id、名字、一共几个）、`rev`（每推送一次加一）；连上时和配置变化时发送）<br>`profile`（按键里的「切换方案」切了方案：`from`、`to`，手机顶栏闪一下）<br>`layoutSaved`（`ok`，没存上时 `error` 说明原因；存好时先推新的 `config` 再回这条） |

status.json 里和手机有关的：`device`、`phoneModel`、`phoneApp`、`configRev`（最后推送的）、`ackRev`、`ackAt`、`configError`（配置读坏时的说明）、`bluetoothError`。

## 已验证

| 项目 | 结果 |
|---|---|
| 后台内存 | 工作集约 15 MB，私有约 3 MB |
| 主界面内存 | 工作集约 34 MB，关掉窗口就释放 |
| 中文显示 | 正常 |
| 布局同步 | 电脑上改配置，手机 1 秒内更新 |
| 断线重连 | 电脑后台重启、手机 App 重装后，都能自动重连 |
| 语音按钮（系统语音引擎） | 按住说话，松手后约 0.7 秒文字粘贴到电脑 |
| 本地识别：模型加载 | App 启动时后台预加载，480ms 约 4.4 秒、960ms 约 5.6 秒；进程内存约 360～390 MB，退到后台 3 分钟后释放 |
| 本地识别：速度 | 小米 13 上 RTF 约 0.1（2 线程），松手后收尾约 0.1 秒 |
| 本地识别：效果 | 27.8 秒的中英混说测试音频，标点、句号、问号齐全，电脑上收到的和手机识别的一字不差；中间停顿 3 秒、6 秒都不断 |
| 边说边打字 | 每 0.5 秒左右打出一段，比说话晚 0.5～1 秒 |
| 按键长按 | 「删除」长按 → Ctrl+Backspace 正常 |
| 连发 | 记事本里按住删除 1.5 秒删 23 个字（1 + 22），← 1 秒、换行 0.7 秒同样准；连发间隔改 100 毫秒后 1.5 秒删 12 个；「最近动作」合成「删除 ×23」 |
| 手机上调布局 | 移动、拖边改大小、不对齐的小数宽度（0.63 格）都写回了配置，电脑主界面自动重新载入；取消不改任何东西；调整时按键没有发给电脑 |
| 电脑上拖边框 | 四条边、四个角的鼠标形状正确；对齐拖、按住 Alt 拖、输入数值、加减按钮、放不下时的红框和提示都正常 |
| 同时改 | 主界面改了还没保存时文件被改，两边的改动都留下 |
| 直通（0.4.0） | 按住设成 Shift 直通的键，记事本里 Shift+← 选中 3 个字；松手后 Ctrl / Shift / Alt / Win 都没按着；直通 ← 按住 1.5 秒移动 14 格；按住时 App 切到后台、改配置都会马上松开 |
| 重复长按（0.4.0） | 按住 1.5 秒触发 9 次长按（500 毫秒一次，350 毫秒后再一次，之后每 100 毫秒）；松手不再算单击 |
| 等待（0.4.0） | 「输入 A → 等 3 秒 → 输入 B」，等待期间按的 ← 立刻执行；等待中改配置，B 不再输入；停用的步骤跳过 |
| 打开网站 / 应用（0.4.0） | example.com 在 Edge 里打开并调到前台；字符映射表启动后在前台，最小化后再按切回同一个窗口（没开第二个） |
| 暂停（0.4.0） | 托盘、主界面顶栏都能暂停 / 恢复；暂停时手机按键变淡、按了不执行 |
| 导入遥控器映射（0.4.0） | 从本机读出遥控器映射软件的 13 个键，预览、确认后新建「遥控器」页，手机上马上能用 |
| 方案（0.5.0） | 新增、切换、导入成新方案；手机上按「切换方案」的键在两个方案之间来回切，桌面通知、手机顶栏都对；旧版的配置代码读新配置不丢方案、序列 |
| 撤销重做（0.5.0） | Ctrl+Z、Ctrl+Y、Ctrl+Shift+Z、顶栏按钮都对；对话框关掉以后快捷键照样能用；托盘暂停以后再撤销，暂停保留 |
| 复制粘贴、预设、序列、录制（0.5.0） | 格式错的提示第几行；引用块里的能粘贴；Ctrl+Shift+K 录得进 |
| 导出导入（0.5.0） | 坏文件整份拒绝并说明是哪个方案哪个键第几步；导入前备份、导入后可撤销 |
| 电量（0.5.0） | 连上就显示 100% 充电中；模拟 42% 不充电（dumpsys battery，测完 reset）界面跟着变 |
| 手机信息（0.6.0） | 后台日志「🤝 少爷的mi13（2211133C，App 0.8.0）」；顶栏、关于页、手机信息浮层显示蓝牙名、型号、App 版本 |
| 配置回执（0.6.0） | 推送 1、2、3 次，手机都在 1 秒内回执（configRev / ackRev 1/1、2/2、3/3）；首页「已同步」「等手机确认…」「手机没有确认（可能是旧版 App）」三种状态都对 |
| 手机上暂停 / 恢复（0.6.0） | 手机上点暂停、恢复，配置里的 `settings.enabled` 跟着变，日志「⏸ 手机：暂停按键」「▶ 手机：恢复按键」，手机 0.4 秒内收到新配置 |
| 剪贴板方式输入（0.6.0） | 真剪贴板、真按 Ctrl+V 粘到测试窗口：用户的微信截图（16 种格式，462×87）、WinForms 放的图片（321×123）、复制的两个文件，粘贴后每种格式逐字节一样、图片尺寸一样、文件列表一样；粘贴后 5 毫秒内另外复制了文字，没被盖掉。单元测试在单独的窗口站里覆盖 HTML、RTF、PNG、DIB、CF_HDROP、自定义格式、增强图元文件、连着粘贴、空剪贴板、私有格式时改成逐字输入 |
| 配置读坏（0.6.0） | 写坏配置后：坏文件留成 `config.broken-时间.json`，原文件不动，横幅写明第几行；「从最近的备份恢复」恢复成功。后台：读坏时留着上一份好的配置、写 `configError`，坏着重启时用最近的自动备份，修好后横幅消失（单元测试） |
| 第一次用方案（0.6.0） | 新增的方案是空的，复制的方案带着映射；没有方案的旧配置读进来变成「默认方案」 |
| 布局（0.6.0） | 这一页满了时新按键放到「第 2 页」并能撤销；拖到一样大的按键上互换位置；删除页面前确认、Esc 关掉、删了能撤销 |
| 文件对话框（0.6.0） | 开着保存对话框 3 秒，主界面只用了 0.02 秒 CPU（以前 2.91 秒） |
| 单元测试（0.6.0） | 配置 27 个、主界面 2 个、后台 9 个全过（后台的包括剪贴板 7 个、配置读坏 1 个；真剪贴板的那个要手动跑，见 `clipboard.rs`） |
| 触控板顺滑（0.7.2） | 在手机触控板上注入手势（直线滑动、轻点后 0.5 秒 / 0.15 秒接着滑、快速左右来回划），后台只记录不动鼠标。同一段真实蓝牙时序：以前 13 段滑动段段都有停顿，按 120 Hz 算停顿帧 51/404，每帧走的距离忽大忽小（变异系数 0.52）；现在 13 段里 10 段一帧不停，停顿帧 5/372，变异系数 0.16。剩下的停顿是蓝牙偶尔七八十到一百五十毫秒什么都不送，指针停一下再接着走，不跳。延迟（指针比蓝牙最快送到时多等的时间）：以前中位 29 毫秒、慢的一成超过 121 毫秒，现在中位 53 毫秒、慢的一成超过 62 毫秒。发布版实测：12 段直线滑动 10 段一帧不停，延迟中位 58 毫秒。后台单元测试 14 个全过（新加回放 5 个） |
| 新版换掉旧窗口（0.7.4） | 隔离副本：开着没挂版本号的 0.7.3 或挂了 0.7.3 的窗口时打开 0.7.4，旧窗口 0.05 秒内关掉，新版接上；开着 0.7.4 时再开 0.7.3 或 0.7.4，后开的自己退出，原窗口不动；前台一直没变。真机：用户开着的 0.7.3 窗口 0.07 秒后关掉，后台跟着换成 0.7.4，手机 1 秒后连回 |
| 内部改名 SuanNIAO（0.7.5） | 真机从旧版换过来：打开新的 SuanNIAO.exe，开着的 PhonePad.exe 窗口和 phonepad-core.exe 0.15 秒内都退出，新后台 0.24 秒后启动；`%APPDATA%\PhonePad` 整个改名成 `%APPDATA%\SuanNIAO`，配置、日志、备份都在；开机自启从 Run 里的 `PhonePad` 换成指向新后台的 `SuanNIAO`；手机（旧 App 0.9.3）1.2 秒后连回，配置回执 1/1。单元测试配置 29 个、后台 14 个、主界面 2 个全过 |
| 手机换新包名（0.9.4） | 真机：另外装上 `dev.suanniao` 调试版，从旧 App 复制设置（校验值一致）、语音模型和测试录音（10 个文件 md5 一致），给好蓝牙、麦克风、通知权限；新 App 不用重新选电脑，自己连上（App 0.9.4，配置回执对上），语音模型 x-asr-480ms 5 秒加载完；卸掉旧 App 后连接不断 |
| 电脑端安装包（0.7.6） | 隔离测试（装到临时目录、不建桌面快捷方式、静默安装）：全新安装 1 秒装好，两个 exe 和编出来的一模一样，开始菜单「蒜鸟」指向安装目录，「设置 → 应用」里是「蒜鸟 0.7.6」。装好的蒜鸟开着（换了内部名字的副本，主界面加后台）时覆盖安装：安装程序先运行 `--quit`，0.14 秒把两个都关掉，再换文件，没弹「文件被占用」。卸载同样先关掉；指向安装目录的开机自启删掉，指向别处的不动；设置文件夹留着。别的文件夹里开着的蒜鸟（`pc\target\release` 里正在用的那份）全程没被关掉。安装界面截图：中文正常，按钮写「安装」，完成页是蒜鸟竖图，没点「完成」就不会打开蒜鸟。单元测试配置 29 个、后台 14 个、主界面 2 个全过 |
| 手机安装包带模型（0.9.5） | 真机：带「快」模型的安装包 162 MB，装好不用推模型；模型直接从安装包里读，5.0 秒加载完（两次 5003、5029 毫秒），刚加载完进程内存约 427 MB、稳定后 337 MB；「⋯ → 关于 → 开源许可」显示四项说明和许可证全文 |
