// Logging to logcat (tag "linuxapk-tinyalsa").
#pragma once
#ifdef __ANDROID__
#include <android/log.h>
#include <stdio.h>

#define LXA_TAG "linuxapk-tinyalsa"
#define ALOGI(...) __android_log_print(ANDROID_LOG_INFO, LXA_TAG, __VA_ARGS__)
#define ALOGW(...) __android_log_print(ANDROID_LOG_WARN, LXA_TAG, __VA_ARGS__)
#define ALOGE(...) __android_log_print(ANDROID_LOG_ERROR, LXA_TAG, __VA_ARGS__)

#else
#define ALOGI(...) ((void)0)
#define ALOGW(...) ((void)0)
#define ALOGE(...) ((void)0)
#endif
