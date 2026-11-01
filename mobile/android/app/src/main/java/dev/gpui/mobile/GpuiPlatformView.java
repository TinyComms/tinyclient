package dev.gpui.mobile;

/**
 * gpui_ce_mobile looks this class up on every pause, resume and shutdown to manage embedded
 * native views. tinyclient embeds none, so the three calls do nothing; without the class each
 * lookup logged a ClassNotFoundException stack to logcat.
 */
public final class GpuiPlatformView {
    private GpuiPlatformView() {}

    public static void pauseAll() {}

    public static void resumeAll() {}

    public static void disposeAll() {}
}
