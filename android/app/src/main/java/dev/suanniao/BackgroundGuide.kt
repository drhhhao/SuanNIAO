package dev.suanniao

import android.annotation.SuppressLint
import android.app.Activity
import android.app.ActivityManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings

/**
 * 各家手机「别在后台限制蒜鸟」的设置：改哪一项、怎么带用户过去。
 * - 安卓标准的有两项：电池优化白名单（isIgnoringBatteryOptimizations）和「后台受限」（isBackgroundRestricted），读得到。
 *   小米（澎湃 OS / MIUI）的「省电策略 → 无限制」就是白名单，三星和原生安卓的「电池 → 不受限制」也是这两项。
 * - 华为、荣耀、OPPO（一加、realme）、vivo（iQOO）、魅族在它之外还有自己的一层后台管理（启动管理、后台耗电），
 *   状态读不到，只能带用户到那一页、告诉他选哪一项。
 * - 没列到的牌子按安卓标准的走。
 */
object BackgroundGuide {

    private enum class Brand { XIAOMI, HUAWEI, HONOR, OPPO, VIVO, SAMSUNG, MEIZU, OTHER }

    private val brand: Brand by lazy {
        val names = "${Build.MANUFACTURER} ${Build.BRAND}".lowercase()
        when {
            listOf("xiaomi", "redmi", "poco").any { it in names } -> Brand.XIAOMI
            "honor" in names -> Brand.HONOR
            "huawei" in names -> Brand.HUAWEI
            listOf("oppo", "oneplus", "realme").any { it in names } -> Brand.OPPO
            listOf("vivo", "iqoo").any { it in names } -> Brand.VIVO
            "samsung" in names -> Brand.SAMSUNG
            "meizu" in names -> Brand.MEIZU
            else -> Brand.OTHER
        }
    }

    /** 这个牌子在安卓标准的两项之外还有自己的一层后台管理（读不到状态） */
    val ownLayer: Boolean get() = brand in setOf(Brand.HUAWEI, Brand.HONOR, Brand.OPPO, Brand.VIVO, Brand.MEIZU)

    /** 安卓标准的两项都放开了：白名单里有蒜鸟、没被设成「后台受限」 */
    fun free(context: Context): Boolean {
        val whitelisted = context.getSystemService(PowerManager::class.java)?.isIgnoringBatteryOptimizations(context.packageName) == true
        val restricted = Build.VERSION.SDK_INT >= 28 && context.getSystemService(ActivityManager::class.java)?.isBackgroundRestricted == true
        return whitelisted && !restricted
    }

    /** 还值得带用户去设置：标准的两项没放开，或者这个牌子有自己的一层（读不到，只能一直给入口） */
    fun worthOpening(context: Context): Boolean = !free(context) || ownLayer

    /** 设置里该改哪一项，一句话（接在「点「去设置」，」后面） */
    fun howTo(): String = when (brand) {
        Brand.XIAOMI -> "在「省电策略」里选「无限制」"
        Brand.HUAWEI, Brand.HONOR -> "在「应用启动管理」里把蒜鸟改成手动管理，打开「允许后台活动」"
        Brand.OPPO -> "在「耗电管理」里打开「允许后台行为」（有的叫「允许完全后台行为」）"
        Brand.VIVO -> "在「后台耗电管理」里把蒜鸟设成「允许后台高耗电」"
        Brand.SAMSUNG -> "在「电池」里选「不受限制」"
        Brand.MEIZU -> "在「后台管理」里选「允许后台运行」"
        Brand.OTHER -> "在「电池」里选「不受限制」（有的叫「不优化」「允许后台运行」）"
    }

    /**
     * 带用户去改。白名单还没放开：弹系统的「允许」框（小米点了就是「无限制」）；有自己一层的牌子，「允许」框后面垫着那一页，
     * 点完接着改。都打不开就去应用详情（「电池」「耗电」都在那里面）。打开了返回 true
     */
    @SuppressLint("BatteryLife") // 蒜鸟是一直连着电脑的遥控器，用户自己选
    fun open(activity: Activity): Boolean {
        val pkg = activity.packageName
        val details = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", pkg, null))
        val request = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$pkg"))
            .takeIf { activity.getSystemService(PowerManager::class.java)?.isIgnoringBatteryOptimizations(pkg) != true }
        for (page in vendorPages(pkg)) {
            val started = runCatching {
                if (request != null) activity.startActivities(arrayOf(page, request)) else activity.startActivity(page)
            }.isSuccess
            if (started) return true
        }
        val tries = listOfNotNull(request, details, Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
        return tries.any { runCatching { activity.startActivity(it) }.isSuccess }
    }

    /** 各家自己的后台管理页（找不到、不让打开的会抛异常，跳过） */
    private fun vendorPages(pkg: String): List<Intent> {
        fun page(packageName: String, cls: String) = Intent().setComponent(ComponentName(packageName, cls))
        val huawei = listOf(
            page("com.huawei.systemmanager", "com.huawei.systemmanager.startupmgr.ui.StartupNormalAppListActivity"),
            page("com.huawei.systemmanager", "com.huawei.systemmanager.optimize.process.ProtectActivity"),
        )
        return when (brand) {
            Brand.HUAWEI -> huawei
            Brand.HONOR -> listOf(page("com.hihonor.systemmanager", "com.hihonor.systemmanager.startupmgr.ui.StartupNormalAppListActivity")) + huawei
            Brand.VIVO -> listOf(
                page("com.vivo.abe", "com.vivo.applicationbehaviorengine.ui.ExcessivePowerManagerActivity"),
                page("com.iqoo.secure", "com.iqoo.secure.ui.phoneoptimize.BgStartUpManager"),
                page("com.vivo.permissionmanager", "com.vivo.permissionmanager.activity.BgStartUpManagerActivity"),
            )
            Brand.MEIZU -> listOf(Intent("com.meizu.safe.security.SHOW_APPSEC").addCategory(Intent.CATEGORY_DEFAULT).putExtra("packageName", pkg))
            // 小米：白名单就是「无限制」；OPPO、三星和别的：开关在应用详情的「耗电管理」「电池」里
            else -> emptyList()
        }
    }
}
