plugins {
    id("java")
    kotlin("jvm") version "1.9.24"
    id("org.jetbrains.intellij.platform") version "2.1.0"
}

group = "com.rex.ide"
version = "0.1.0"

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    intellijPlatform {
        // Community edition is enough: the plugin is a thin client over the
        // local `rex serve` HTTP API, no IDE-internal machinery needed.
        create("IC", "2024.1")
    }
}

intellijPlatform {
    pluginConfiguration {
        id = "com.rex.ide"
        name = "REX"
        description = "Drive the local REX harness from the IDE: run tasks, approve plans and tool calls, checkpoint and rewind."
        vendor {
            name = "rex corp"
        }
    }
}

tasks {
    withType<JavaCompile> {
        sourceCompatibility = "17"
        targetCompatibility = "17"
    }
    withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile> {
        kotlinOptions.jvmTarget = "17"
    }
}
