plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    // 包名是改名（蒜鸟）前起的：改了就成了另一个 App，手机上的设置、语音模型都得重来，所以不改
    namespace = "dev.phonepad"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.phonepad"
        minSdk = 28
        targetSdk = 35
        versionCode = 18
        versionName = "0.9.3"
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

dependencies {
    // 本地流式语音识别：https://github.com/k2-fsa/sherpa-onnx/releases/tag/v2.0.1
    implementation(files("libs/sherpa-onnx-2.0.1.aar"))
}
