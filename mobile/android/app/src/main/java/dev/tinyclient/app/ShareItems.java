package dev.tinyclient.app;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ShortcutInfo;
import android.content.pm.ShortcutManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.provider.MediaStore;
import android.provider.OpenableColumns;

import androidx.core.content.FileProvider;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.Executor;
import java.util.concurrent.Executors;

import org.json.JSONArray;
import org.json.JSONObject;

/** Copies granted URIs while the grant is valid. Rust receives only private outbox paths. */
final class ShareItems {
    private static final int PICK = 0x7520;
    private static final int CAPTURE = 0x7521;
    private static final int CAMERA_PERMISSION = 0x7522;
    private static final int MAX_FILES = 32;
    private static final long MAX_BYTES = 1024L * 1024 * 1024;
    private static final Executor IO = Executors.newSingleThreadExecutor();
    private static File captured;
    private static boolean captureVideo;

    private ShareItems() {}

    static native void nativeReceived(int kind, String data);

    static void handle(Context context, Intent intent) {
        if (intent == null) return;
        if (Intent.ACTION_VIEW.equals(intent.getAction())) {
            Uri url = intent.getData();
            if (url != null && "tinyclient".equals(url.getScheme())) nativeReceived(2, url.toString());
            return;
        }
        if (Intent.ACTION_SEND.equals(intent.getAction()) || Intent.ACTION_SEND_MULTIPLE.equals(intent.getAction())) {
            // No view access is needed and the activity can keep responding during a large copy.
            IO.execute(() -> deliver(context.getApplicationContext(), intent, 1, null));
        }
    }

    static Set<Uri> uris(Intent intent) {
        LinkedHashSet<Uri> found = new LinkedHashSet<>();
        if (intent == null) return found;
        add(found, intent.getData());
        ClipData clips = intent.getClipData();
        if (clips != null) {
            for (int i = 0; i < clips.getItemCount() && found.size() <= MAX_FILES; i++) {
                add(found, clips.getItemAt(i).getUri());
            }
        }
        Object streams = intent.getExtras() == null ? null : intent.getExtras().get(Intent.EXTRA_STREAM);
        if (streams instanceof Uri) add(found, (Uri) streams);
        if (streams instanceof ArrayList<?>) {
            for (Object item : (ArrayList<?>) streams) {
                if (item instanceof Uri) add(found, (Uri) item);
                if (found.size() > MAX_FILES) break;
            }
        }
        return found;
    }

    private static void add(Set<Uri> found, Uri uri) {
        if (uri != null && "content".equals(uri.getScheme())) found.add(uri);
    }

    private static void deliver(Context context, Intent intent, int kind, File capture) {
        ArrayList<File> copied = new ArrayList<>();
        try {
            JSONObject result = new JSONObject();
            JSONArray files = new JSONArray();
            Set<Uri> shared = uris(intent);
            if (capture != null && capture.length() > 0) {
                File destination = newDestination(context, capture.getName());
                copied.add(destination);
                if (capture.length() > MAX_BYTES || !capture.renameTo(destination)) {
                    throw new IOException("Couldn't keep camera output");
                }
                files.put(destination.getAbsolutePath());
                shared.clear();
            }
            if (shared.size() > MAX_FILES) throw new IOException("Too many files");
            long remaining = MAX_BYTES;
            for (Uri uri : shared) {
                if (kind == 1 && (context.getPackageName() + ".share-files").equals(uri.getAuthority())) {
                    throw new IOException("Share points to a private app file");
                }
                File destination = newDestination(context, fileName(context, uri));
                copied.add(destination);
                remaining -= copy(context, uri, destination, remaining);
                files.put(destination.getAbsolutePath());
            }
            if (files.length() != 0) result.put("files", files);
            if (kind == 1 && intent != null) {
                CharSequence text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT);
                ClipData clips = intent.getClipData();
                if (text == null && clips != null && clips.getItemCount() > 0) text = clips.getItemAt(0).getText();
                if (text != null) result.put("text", text.toString());
                String destination = intent.getStringExtra(Intent.EXTRA_SHORTCUT_ID);
                if (destination != null) result.put("destination", destination);
            }
            nativeReceived(kind, result.toString());
        } catch (Exception error) {
            for (File file : copied) file.delete();
            nativeReceived(kind, "{\"error\":true}");
        } finally {
            if (capture != null) capture.delete();
        }
    }

    private static File newDestination(Context context, String name) throws IOException {
        File folder = new File(context.getFilesDir(), "outbox/phone-" + UUID.randomUUID());
        if (!folder.mkdirs()) throw new IOException("Couldn't create attachment staging");
        return new File(folder, cleanName(name));
    }

    static String cleanName(String name) {
        String clean = name.replaceAll("[/\\\\\\p{Cntrl}]", "_").trim();
        if (clean.isEmpty() || ".".equals(clean) || "..".equals(clean)) return "attachment";
        if (clean.codePointCount(0, clean.length()) <= 60) return clean;
        int dot = clean.lastIndexOf('.');
        String suffix = dot > 0 && clean.length() - dot <= 12 ? clean.substring(dot) : "";
        return clean.substring(0, clean.offsetByCodePoints(0, 48)) + suffix;
    }

    private static String fileName(Context context, Uri uri) {
        try (Cursor result = context.getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (result != null && result.moveToFirst() && !result.isNull(0)) return result.getString(0);
        }
        String type = context.getContentResolver().getType(uri);
        String extension = type == null ? null : android.webkit.MimeTypeMap.getSingleton().getExtensionFromMimeType(type);
        return extension == null ? "attachment" : "attachment." + extension;
    }

    private static long copy(Context context, Uri uri, File destination, long remaining) throws IOException {
        try (InputStream input = context.getContentResolver().openInputStream(uri);
             FileOutputStream output = new FileOutputStream(destination)) {
            if (input == null) throw new IOException("Provider returned no file");
            return transfer(input, output, remaining);
        }
    }

    private static long transfer(InputStream input, FileOutputStream output, long remaining) throws IOException {
            byte[] buffer = new byte[64 * 1024];
            long total = 0;
            int size;
            while ((size = input.read(buffer)) != -1) {
                total += size;
                if (total > remaining) throw new IOException("Share exceeds staging size");
                output.write(buffer, 0, size);
            }
            return total;
    }

    public static boolean pick() {
        if (TinyPlatform.activity() == null) return false;
        TinyPlatform.ui().post(() -> {
            Activity activity = TinyPlatform.activity();
            if (activity == null || activity.isFinishing()) {
                nativeReceived(0, "{}");
                return;
            }
            new AlertDialog.Builder(activity).setTitle("Attach")
                    .setItems(new String[]{"Photos and videos", "Take photo", "Record video", "Documents"},
                            (dialog, choice) -> {
                                try {
                                    if (choice == 1 || choice == 2) {
                                        captureVideo = choice == 2;
                                        if (activity.checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
                                            activity.requestPermissions(new String[]{Manifest.permission.CAMERA}, CAMERA_PERMISSION);
                                        } else {
                                            openCamera(activity);
                                        }
                                    } else {
                                        Intent picker = picker(choice == 0);
                                        activity.startActivityForResult(picker, PICK);
                                    }
                                } catch (Exception unavailable) {
                                    nativeReceived(0, "{\"error\":true}");
                                }
                            })
                    .setOnCancelListener(dialog -> nativeReceived(0, "{}"))
                    .setNegativeButton("Cancel", (dialog, which) -> nativeReceived(0, "{}"))
                    .show();
        });
        return true;
    }

    private static Intent picker(boolean photos) {
        if (photos && Build.VERSION.SDK_INT >= 33) {
            return new Intent(MediaStore.ACTION_PICK_IMAGES)
                    .setType("*/*").putExtra(MediaStore.EXTRA_PICK_IMAGES_MAX, Math.min(MAX_FILES, MediaStore.getPickImagesMaxLimit()));
        }
        Intent picker = new Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE)
                .setType("*/*").putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        if (photos) picker.putExtra(Intent.EXTRA_MIME_TYPES, new String[]{"image/*", "video/*"});
        return picker;
    }

    private static void openCamera(Activity activity) throws IOException {
        File folder = new File(activity.getCacheDir(), "phone-capture");
        if (!folder.isDirectory() && !folder.mkdirs()) throw new IOException("Couldn't open camera storage");
        captured = new File(folder, "Capture-" + UUID.randomUUID() + (captureVideo ? ".mp4" : ".jpg"));
        Uri uri = FileProvider.getUriForFile(activity, activity.getPackageName() + ".share-files", captured);
        Intent request = new Intent(captureVideo ? MediaStore.ACTION_VIDEO_CAPTURE : MediaStore.ACTION_IMAGE_CAPTURE)
                .putExtra(MediaStore.EXTRA_OUTPUT, uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
        request.setClipData(ClipData.newRawUri("Capture", uri));
        activity.startActivityForResult(request, CAPTURE);
    }

    static void onPermissions(Activity activity, int request, int[] results) {
        if (request != CAMERA_PERMISSION) return;
        try {
            if (results.length == 0 || results[0] != PackageManager.PERMISSION_GRANTED) throw new IOException("Camera denied");
            openCamera(activity);
        } catch (Exception unavailable) {
            nativeReceived(0, "{\"error\":true}");
        }
    }

    static void onResult(Context context, int request, int result, Intent data) {
        if (request != PICK && request != CAPTURE) return;
        File capture = request == CAPTURE ? captured : null;
        if (request == CAPTURE) captured = null;
        if (result == Activity.RESULT_OK) {
            IO.execute(() -> deliver(context.getApplicationContext(), data, 0, capture));
        } else {
            if (capture != null) capture.delete();
            nativeReceived(0, "{}");
        }
    }

    public static void recent(String id, String title) {
        if (Build.VERSION.SDK_INT < 30) return;
        Context context = TinyPlatform.context();
        if (context == null) return;
        TinyPlatform.ui().post(() -> {
            ShortcutManager manager = context.getSystemService(ShortcutManager.class);
            if (manager == null) return;
            Intent activity = new Intent(context, TinyActivity.class).setAction(Intent.ACTION_MAIN);
            manager.pushDynamicShortcut(new ShortcutInfo.Builder(context, id).setShortLabel(title)
                    .setIntent(activity).setLongLived(true).setCategories(Set.of("dev.tinyclient.app.SHARE_CHAT")).build());
        });
    }

    public static void share(String source, String name) {
        Context context = TinyPlatform.context();
        if (context == null) return;
        IO.execute(() -> {
            try {
                File root = new File(context.getCacheDir(), "phone-share");
                File[] old = root.listFiles();
                if (old != null) {
                    long cutoff = System.currentTimeMillis() - 24 * 60 * 60 * 1000L;
                    for (File file : old) {
                        if (file.lastModified() < cutoff) {
                            File[] copies = file.listFiles();
                            if (copies != null) for (File copy : copies) copy.delete();
                            file.delete();
                        }
                    }
                }
                File folder = new File(root, UUID.randomUUID().toString());
                if (!folder.mkdirs()) throw new IOException("Couldn't prepare share");
                File destination = new File(folder, cleanName(name));
                try (FileInputStream input = new FileInputStream(source);
                     FileOutputStream output = new FileOutputStream(destination)) {
                    transfer(input, output, MAX_BYTES);
                } catch (IOException failure) {
                    destination.delete();
                    throw failure;
                }
                Uri uri = FileProvider.getUriForFile(context, context.getPackageName() + ".share-files", destination);
                String mime = android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(
                        android.webkit.MimeTypeMap.getFileExtensionFromUrl(destination.getName()).toLowerCase(java.util.Locale.ROOT));
                Intent send = new Intent(Intent.ACTION_SEND).setType(mime == null ? "application/octet-stream" : mime)
                        .putExtra(Intent.EXTRA_STREAM, uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                send.setClipData(ClipData.newRawUri(name, uri));
                TinyPlatform.ui().post(() -> {
                    Activity activity = TinyPlatform.activity();
                    if (activity != null && !activity.isFinishing()) activity.startActivity(Intent.createChooser(send, null));
                });
            } catch (Exception failure) {
                TinyPlatform.ui().post(() -> android.widget.Toast.makeText(context, "Couldn't share the attachment.", android.widget.Toast.LENGTH_SHORT).show());
            }
        });
    }
}
