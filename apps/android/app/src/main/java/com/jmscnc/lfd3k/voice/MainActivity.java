package com.jmscnc.lfd3k.voice;

import android.Manifest;
import android.app.NativeActivity;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.PackageManager;
import android.hardware.usb.UsbConstants;
import android.hardware.usb.UsbDevice;
import android.hardware.usb.UsbDeviceConnection;
import android.hardware.usb.UsbEndpoint;
import android.hardware.usb.UsbInterface;
import android.hardware.usb.UsbManager;
import android.net.wifi.WifiManager;
import android.content.res.Configuration;
import android.graphics.Insets;
import android.os.Build;
import android.os.Bundle;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowManager;

import java.util.HashMap;

/**
 * The Java that Android will not let a native app skip: the USB host
 * permission dialog, claiming the interface so the fd stays open, the
 * Wi-Fi multicast lock mDNS needs, and the microphone runtime permission.
 * The voice engine and the UI are native.
 */
public class MainActivity extends NativeActivity {
    private static final String USB_ACTION = "com.jmscnc.lfd3k.voice.USB_PERMISSION";
    private static final int VID_ESPRESSIF = 0x303A;
    private static final int VID_CP210X = 0x10C4;

    private UsbDeviceConnection connection;
    private volatile boolean usbWaiting;
    private volatile String usbError = "";
    private WifiManager.MulticastLock multicast;
    private final int[] insetsPx = new int[4];

    private final BroadcastReceiver usbReceiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            if (intent == null || !USB_ACTION.equals(intent.getAction())) return;
            usbWaiting = false;
            boolean granted = intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false);
            if (!granted) usbError = "USB permission was denied";
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        IntentFilter filter = new IntentFilter(USB_ACTION);
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(usbReceiver, filter, Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(usbReceiver, filter);
        }
        requestAudio();
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams lp = getWindow().getAttributes();
            lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            getWindow().setAttributes(lp);
        }
        View decor = getWindow().getDecorView();
        decor.setOnApplyWindowInsetsListener((v, windowInsets) -> {
            cacheInsets(windowInsets);
            return v.onApplyWindowInsets(windowInsets);
        });
        decor.requestApplyInsets();
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        requestInsets();
    }

    public int densityDpi() {
        int d = getResources().getConfiguration().densityDpi;
        return d < 120 ? 160 : d;
    }

    public float fontScale() {
        float f = getResources().getConfiguration().fontScale;
        if (f < 0.5f || f > 3.f) return 1.f;
        return f;
    }

    public int[] windowInsets() {
        int[] copy = new int[4];
        synchronized (insetsPx) {
            System.arraycopy(insetsPx, 0, copy, 0, 4);
        }
        return copy;
    }

    public void requestInsets() {
        runOnUiThread(() -> {
            View decor = getWindow().getDecorView();
            if (decor != null) decor.requestApplyInsets();
        });
    }

    private void cacheInsets(WindowInsets windowInsets) {
        int left;
        int top;
        int right;
        int bottom;
        if (Build.VERSION.SDK_INT >= 30) {
            Insets bars = windowInsets.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            left = bars.left;
            top = bars.top;
            right = bars.right;
            bottom = bars.bottom;
        } else {
            left = windowInsets.getSystemWindowInsetLeft();
            top = windowInsets.getSystemWindowInsetTop();
            right = windowInsets.getSystemWindowInsetRight();
            bottom = windowInsets.getSystemWindowInsetBottom();
        }
        synchronized (insetsPx) {
            insetsPx[0] = Math.max(0, left);
            insetsPx[1] = Math.max(0, top);
            insetsPx[2] = Math.max(0, right);
            insetsPx[3] = Math.max(0, bottom);
        }
    }

    @Override
    protected void onDestroy() {
        usbClose();
        releaseMulticast();
        try {
            unregisterReceiver(usbReceiver);
        } catch (IllegalArgumentException ignored) {
            // Already gone.
        }
        super.onDestroy();
    }

    public boolean audioGranted() {
        return checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED;
    }

    public void requestAudio() {
        if (!audioGranted()) {
            requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO}, 1);
        }
    }

    public String filesPath() {
        return getFilesDir().getAbsolutePath();
    }

    public String extra(String key) {
        Intent intent = getIntent();
        if (intent == null || key == null) return "";
        String v = intent.getStringExtra(key);
        return v == null ? "" : v;
    }

    public void keepScreen(boolean on) {
        runOnUiThread(() -> {
            if (on) getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            else getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        });
    }

    public void acquireMulticast() {
        WifiManager wm = (WifiManager) getApplicationContext().getSystemService(WIFI_SERVICE);
        if (wm == null) return;
        if (multicast == null) {
            multicast = wm.createMulticastLock("lfd3k-voice");
            multicast.setReferenceCounted(false);
        }
        if (!multicast.isHeld()) multicast.acquire();
    }

    public void releaseMulticast() {
        if (multicast != null && multicast.isHeld()) multicast.release();
    }

    public String usbLastError() {
        return usbError == null ? "" : usbError;
    }

    /**
     * out: fd, bulk in, bulk out, data interface, control interface, kind
     * (1 CDC-ACM, 2 CP210x). Returns 0 when the fd is open, 1 when the
     * permission dialog is up, negative on error.
     */
    public int usbPrepare(int[] out) {
        if (out == null || out.length < 6) return -1;
        UsbManager mgr = (UsbManager) getSystemService(USB_SERVICE);
        if (mgr == null) {
            usbError = "this phone has no USB host service";
            return -2;
        }
        HashMap<String, UsbDevice> list = mgr.getDeviceList();
        UsbDevice dev = pick(list);
        if (dev == null) {
            usbError = "no Heltec modem on USB (CDC-ACM, Espressif 303A, or CP210x)";
            return -3;
        }
        if (!mgr.hasPermission(dev)) {
            usbError = "allow USB access for the modem";
            if (!usbWaiting) {
                usbWaiting = true;
                int flags = PendingIntent.FLAG_UPDATE_CURRENT;
                if (Build.VERSION.SDK_INT >= 31) flags |= PendingIntent.FLAG_MUTABLE;
                PendingIntent pi = PendingIntent.getBroadcast(this, 0, new Intent(USB_ACTION), flags);
                mgr.requestPermission(dev, pi);
            }
            return 1;
        }
        usbWaiting = false;
        int[] found = new int[6];
        if (!describe(dev, found)) {
            usbError = "the USB device has no bulk in and out endpoints";
            return -4;
        }
        UsbDeviceConnection conn = mgr.openDevice(dev);
        if (conn == null) {
            usbError = "could not open the USB device";
            return -5;
        }
        UsbInterface data = dev.getInterface(found[3]);
        if (!conn.claimInterface(data, true)) {
            conn.close();
            usbError = "could not claim the USB data interface";
            return -6;
        }
        if (found[4] != found[3]) {
            conn.claimInterface(dev.getInterface(found[4]), true);
        }
        if (connection != null) connection.close();
        connection = conn;
        found[0] = conn.getFileDescriptor();
        System.arraycopy(found, 0, out, 0, 6);
        usbError = "";
        return 0;
    }

    public void usbClose() {
        if (connection != null) {
            connection.close();
            connection = null;
        }
    }

    private UsbDevice pick(HashMap<String, UsbDevice> list) {
        if (list == null || list.isEmpty()) return null;
        UsbDevice best = null;
        int bestScore = 0;
        for (UsbDevice d : list.values()) {
            int score = 0;
            if (d.getVendorId() == VID_ESPRESSIF) score = 30;
            else if (d.getVendorId() == VID_CP210X) score = 20;
            else if (hasCdc(d)) score = 10;
            else if (hasBulkPair(d)) score = 1;
            if (score > bestScore) {
                bestScore = score;
                best = d;
            }
        }
        return best;
    }

    private static boolean hasCdc(UsbDevice d) {
        for (int i = 0; i < d.getInterfaceCount(); i++) {
            int cls = d.getInterface(i).getInterfaceClass();
            if (cls == UsbConstants.USB_CLASS_COMM || cls == UsbConstants.USB_CLASS_CDC_DATA) return true;
        }
        return false;
    }

    private static boolean hasBulkPair(UsbDevice d) {
        int[] tmp = new int[6];
        return describe(d, tmp);
    }

    /** Fills ep in, ep out, data interface, control interface, kind. */
    private static boolean describe(UsbDevice d, int[] out) {
        int epIn = -1, epOut = -1, data = -1, ctrl = -1;
        int cdcData = -1, cdcIn = -1, cdcOut = -1;
        for (int i = 0; i < d.getInterfaceCount(); i++) {
            UsbInterface inf = d.getInterface(i);
            int cls = inf.getInterfaceClass();
            if (cls == UsbConstants.USB_CLASS_COMM && ctrl < 0) ctrl = i;
            int in = -1, outEp = -1;
            for (int e = 0; e < inf.getEndpointCount(); e++) {
                UsbEndpoint ep = inf.getEndpoint(e);
                if (ep.getType() != UsbConstants.USB_ENDPOINT_XFER_BULK) continue;
                if (ep.getDirection() == UsbConstants.USB_DIR_IN) in = ep.getAddress();
                else outEp = ep.getAddress();
            }
            if (in >= 0 && outEp >= 0) {
                if (cls == UsbConstants.USB_CLASS_CDC_DATA) {
                    cdcData = i;
                    cdcIn = in;
                    cdcOut = outEp;
                }
                if (data < 0) {
                    data = i;
                    epIn = in;
                    epOut = outEp;
                }
            }
        }
        if (cdcData >= 0) {
            data = cdcData;
            epIn = cdcIn;
            epOut = cdcOut;
        }
        if (data < 0) return false;
        if (ctrl < 0) ctrl = data;
        out[1] = epIn;
        out[2] = epOut;
        out[3] = data;
        out[4] = ctrl;
        out[5] = d.getVendorId() == VID_CP210X ? 2 : 1;
        return true;
    }
}
