plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    // 包名定了就别再改：改了就成了另一个 App，装着旧包的手机升级不上来，设置、语音模型都得重来。
    // 改名前叫 dev.phonepad（0.9.3 以前）
    namespace = "dev.suanniao"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.suanniao"
        minSdk = 28
        targetSdk = 35
        versionCode = 34
        versionName = "0.9.19"
        // 只给 64 位 ARM 手机用，本地语音识别的原生库只打这一种，安装包小很多
        ndk { abiFilters += "arm64-v8a" }
    }

    buildFeatures {
        buildConfig = true
    }

    // 发版签名：密钥在 GitHub 的加密变量里，发版时由 .github/workflows/release.yml 还原成文件、用环境变量传进来。
    // 本机平时打 debug 包用不到；没设这些变量时 release 包不签名。
    signingConfigs {
        System.getenv("SUANNIAO_KEYSTORE")?.let { path ->
            create("release") {
                storeFile = file(path)
                storePassword = System.getenv("SUANNIAO_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("SUANNIAO_KEY_ALIAS")
                keyPassword = System.getenv("SUANNIAO_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.findByName("release")
        }
    }

    packaging {
        // sherpa-onnx 的 C / C++ 接口库，Kotlin 走 JNI 用不到
        jniLibs.excludes += listOf("**/libsherpa-onnx-c-api.so", "**/libsherpa-onnx-cxx-api.so")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

// 正式版安装包里带上「快」那档语音模型（x-asr-480ms），装好就能用，不用另外下载。
// 模型在仓库外面的 ../models/ 里（太大，进不了 GitHub），发版时 release.yml 先把它下载好。
// 本机打的 debug 包默认不带（装得快），要带就加 -PbundleModel。
val bundledModel = rootProject.file("../models/x-asr-480ms")
val bundledModelFiles = listOf("encoder.int8.onnx", "decoder.onnx", "joiner.int8.onnx", "tokens.txt")

/** 把模型的四个文件放到 assets 的 models/<模型名>/ 下 */
abstract class StageModel : DefaultTask() {
    @get:Input abstract val modelName: Property<String>
    @get:InputFiles abstract val modelFiles: ConfigurableFileCollection
    @get:OutputDirectory abstract val outputDir: DirectoryProperty

    @TaskAction
    fun stage() {
        val missing = modelFiles.files.filterNot { it.isFile }
        if (missing.isNotEmpty()) throw GradleException("要打进安装包的语音模型缺文件：${missing.joinToString { it.path }}")
        val dir = outputDir.get().asFile.resolve("models/${modelName.get()}")
        dir.deleteRecursively()
        dir.mkdirs()
        modelFiles.files.forEach { it.copyTo(dir.resolve(it.name)) }
    }
}

androidComponents {
    onVariants { variant ->
        if (variant.buildType != "release" && !providers.gradleProperty("bundleModel").isPresent) return@onVariants
        val stage = tasks.register<StageModel>("stage${variant.name.replaceFirstChar { it.uppercase() }}Model") {
            modelName.set(bundledModel.name)
            modelFiles.from(bundledModelFiles.map { bundledModel.resolve(it) })
        }
        variant.sources.assets?.addGeneratedSourceDirectory(stage, StageModel::outputDir)
    }
}

dependencies {
    // 本地流式语音识别：https://github.com/k2-fsa/sherpa-onnx/releases/tag/v2.0.1
    implementation(files("libs/sherpa-onnx-2.0.1.aar"))
    // 扫电脑上的二维码（局域网绑定）：https://github.com/journeyapps/zxing-android-embedded
    implementation("com.journeyapps:zxing-android-embedded:4.3.0") { isTransitive = false }
    implementation("com.google.zxing:core:3.5.3")
    // 扫码库申请相机权限时用到（ActivityCompat / ContextCompat）。只要它本身：带上依赖会多出协程、lifecycle 等好几 MB
    implementation("androidx.core:core:1.13.1") { isTransitive = false }
}
