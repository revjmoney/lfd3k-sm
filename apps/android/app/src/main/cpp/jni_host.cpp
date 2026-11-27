#include "jni_host.h"

#include <android/log.h>
#include <android_native_app_glue.h>
#include <jni.h>

#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "lfd3k", __VA_ARGS__)

namespace {

JNIEnv *env_of(android_app *app) {
    JNIEnv *env = nullptr;
    if (!app || !app->activity || !app->activity->vm) return nullptr;
    JavaVM *vm = app->activity->vm;
    if (vm->GetEnv(reinterpret_cast<void **>(&env), JNI_VERSION_1_6) == JNI_EDETACHED) {
        if (vm->AttachCurrentThread(&env, nullptr) != JNI_OK) return nullptr;
    }
    return env;
}

void clear_exc(JNIEnv *env) {
    if (env && env->ExceptionCheck()) {
        env->ExceptionDescribe();
        env->ExceptionClear();
    }
}

jmethodID method(JNIEnv *env, jobject obj, const char *name, const char *sig) {
    jclass cls = env->GetObjectClass(obj);
    if (!cls) return nullptr;
    jmethodID id = env->GetMethodID(cls, name, sig);
    env->DeleteLocalRef(cls);
    clear_exc(env);
    return id;
}

std::string jstring_to(JNIEnv *env, jstring js) {
    if (!js) return {};
    const char *c = env->GetStringUTFChars(js, nullptr);
    std::string out = c ? c : "";
    if (c) env->ReleaseStringUTFChars(js, c);
    env->DeleteLocalRef(js);
    return out;
}

}  // namespace

int jni_usb_prepare(android_app *app, int out[6], std::string *err) {
    JNIEnv *env = env_of(app);
    if (!env) {
        if (err) *err = "no Java VM";
        return -1;
    }
    jobject obj = app->activity->clazz;
    jmethodID id = method(env, obj, "usbPrepare", "([I)I");
    if (!id) {
        if (err) *err = "usbPrepare is missing";
        return -1;
    }
    jintArray arr = env->NewIntArray(6);
    jint rc = env->CallIntMethod(obj, id, arr);
    if (env->ExceptionCheck()) {
        clear_exc(env);
        if (err) *err = "usbPrepare threw";
        env->DeleteLocalRef(arr);
        return -1;
    }
    jint tmp[6] = {};
    env->GetIntArrayRegion(arr, 0, 6, tmp);
    env->DeleteLocalRef(arr);
    for (int i = 0; i < 6; i++) out[i] = tmp[i];
    if (rc != 0) {
        jmethodID e = method(env, obj, "usbLastError", "()Ljava/lang/String;");
        if (e && err) {
            *err = jstring_to(env, (jstring)env->CallObjectMethod(obj, e));
            clear_exc(env);
        }
    }
    return rc;
}

void jni_usb_close(android_app *app) {
    JNIEnv *env = env_of(app);
    if (!env) return;
    jobject obj = app->activity->clazz;
    jmethodID id = method(env, obj, "usbClose", "()V");
    if (id) env->CallVoidMethod(obj, id);
    clear_exc(env);
}

void jni_multicast(android_app *app, bool acquire) {
    JNIEnv *env = env_of(app);
    if (!env) return;
    jobject obj = app->activity->clazz;
    const char *name = acquire ? "acquireMulticast" : "releaseMulticast";
    jmethodID id = method(env, obj, name, "()V");
    if (id) env->CallVoidMethod(obj, id);
    clear_exc(env);
}

bool jni_audio_granted(android_app *app) {
    JNIEnv *env = env_of(app);
    if (!env) return false;
    jobject obj = app->activity->clazz;
    jmethodID id = method(env, obj, "audioGranted", "()Z");
    if (!id) return false;
    jboolean v = env->CallBooleanMethod(obj, id);
    clear_exc(env);
    return v == JNI_TRUE;
}

void jni_request_audio(android_app *app) {
    JNIEnv *env = env_of(app);
    if (!env) return;
    jobject obj = app->activity->clazz;
    jmethodID id = method(env, obj, "requestAudio", "()V");
    if (id) env->CallVoidMethod(obj, id);
    clear_exc(env);
}

std::string jni_files(android_app *app) {
    if (app && app->activity && app->activity->internalDataPath) {
        return app->activity->internalDataPath;
    }
    JNIEnv *env = env_of(app);
    if (!env) return ".";
    jobject obj = app->activity->clazz;
    jmethodID id = method(env, obj, "filesPath", "()Ljava/lang/String;");
    if (!id) return ".";
    std::string s = jstring_to(env, (jstring)env->CallObjectMethod(obj, id));
    clear_exc(env);
    return s.empty() ? "." : s;
}

std::string jni_extra(android_app *app, const char *key) {
    JNIEnv *env = env_of(app);
    if (!env || !key) return {};
    jobject obj = app->activity->clazz;
    jmethodID id = method(env, obj, "extra", "(Ljava/lang/String;)Ljava/lang/String;");
    if (!id) return {};
    jstring ks = env->NewStringUTF(key);
    std::string s = jstring_to(env, (jstring)env->CallObjectMethod(obj, id, ks));
    env->DeleteLocalRef(ks);
    clear_exc(env);
    return s;
}

void jni_keep_screen(android_app *app, bool on) {
    JNIEnv *env = env_of(app);
    if (!env) return;
    jobject obj = app->activity->clazz;
    jmethodID id = method(env, obj, "keepScreen", "(Z)V");
    if (id) env->CallVoidMethod(obj, id, on ? JNI_TRUE : JNI_FALSE);
    clear_exc(env);
}
