package app.coomi;

import com.termux.BuildConfig;

import android.annotation.SuppressLint;
import android.app.AlertDialog;
import android.app.DownloadManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.pm.Signature;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.os.Handler;
import android.os.Looper;
import android.widget.Toast;

import androidx.core.content.FileProvider;

import java.io.File;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;

import org.json.JSONObject;

/**
 * 软件内检查更新（第三批 6）：
 * 读取 updates.septemc.com/coomi/android/latest.json，与当前 versionCode 比较，
 * 有新版则提示并下载 APK（DownloadManager → 私有 files 目录），完成后经
 * FileProvider 唤起系统安装器。
 */
public final class UpdateChecker {

    /**
     * 更新源改为 GitHub 仓库内的元数据 + GitHub Release 资产，统一经 gh-proxy.com 加速。
     * 不再依赖自建服务器下载源。
     */
    private static final String UPDATE_BASE =
        "https://gh-proxy.com/https://raw.githubusercontent.com/TensorHub-ORG/Coomi/main/deploy/updates/android";
    private static final String TEST_UPDATE_BASE =
        "https://gh-proxy.com/https://raw.githubusercontent.com/TensorHub-ORG/Coomi/main/deploy/updates/android-test";
    private static final String UPDATE_URL = UPDATE_BASE + "/latest.json";
    private static final String TEST_UPDATE_URL = TEST_UPDATE_BASE + "/latest.json";
    private static final String PREFS_NAME = "coomi_settings";
    private static final String KEY_TEST_UPDATE_DOT = "test_update_dot_enabled";
    private static final String KEY_UPDATE_REMINDERS = "update_reminders_enabled";
    private static final String TAG = "UpdateChecker";

    public interface Callback {
        void onResult(boolean hasUpdate, String version, String notes, String error);
    }

    private UpdateChecker() {}

    public static int currentVersionCode(Context context) {
        try {
            return context.getPackageManager()
                .getPackageInfo(context.getPackageName(), 0).versionCode;
        } catch (PackageManager.NameNotFoundException e) {
            return 0;
        }
    }


    public static boolean remindersEnabled(Context context) {
        return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).getBoolean(KEY_UPDATE_REMINDERS, true);
    }
    public static void setRemindersEnabled(Context context, boolean enabled) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit().putBoolean(KEY_UPDATE_REMINDERS, enabled).apply();
    }

    /** 测试通道更新是否也亮红点（默认开：内置更新会主动发现测试版并提示）。 */
    public static boolean isTestUpdateDotEnabled(Context context) {
        return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(KEY_TEST_UPDATE_DOT, true);
    }

    public static void setTestUpdateDotEnabled(Context context, boolean enabled) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit().putBoolean(KEY_TEST_UPDATE_DOT, enabled).apply();
    }

    /** 拉取单个通道 latest.json；任何失败返回 null（不阻断另一通道的判断）。 */
    private static JSONObject fetchLatest(String url, int currentVersionCode) {
        try {
            URL target = new URL(url);
            HttpURLConnection conn = (HttpURLConnection) target.openConnection();
            conn.setConnectTimeout(8000);
            conn.setReadTimeout(8000);
            conn.setRequestProperty("User-Agent", "Coomi-Android/" + currentVersionCode);
            int code = conn.getResponseCode();
            if (code != 200) return null;
            try (InputStream in = conn.getInputStream()) {
                java.io.ByteArrayOutputStream buffer = new java.io.ByteArrayOutputStream();
                byte[] chunk = new byte[8192];
                int n;
                while ((n = in.read(chunk)) >= 0) buffer.write(chunk, 0, n);
                return new JSONObject(new String(buffer.toByteArray(), StandardCharsets.UTF_8));
            }
        } catch (Exception e) {
            return null;
        }
    }

    /** latest.json 里的 url 优先（GitHub Release 资产），旧格式回退到 file + 通道目录。 */
    private static String releaseApkUrl(JSONObject release, String baseUrl) {
        if (release == null) return "";
        String direct = release.optString("url", "");
        if (!direct.isEmpty()) return direct;
        String file = release.optString("file", "");
        return file.isEmpty() ? "" : baseUrl + "/" + file;
    }

    /**
     * 静默红点检查（双通道策略）：
     * - 两个通道都会检查，取版本号更高的一个；
     * - 测试通道有更新时提示「【测试版】」，测试红点可由开关控制（默认开）。
     */
    public static void checkSilent(final Context context, final Callback callback) {
        final int current = currentVersionCode(context);
        new Thread(() -> {
            JSONObject stable = fetchLatest(UPDATE_URL, current);
            JSONObject test = fetchLatest(TEST_UPDATE_URL, current);
            int stableCode = stable == null ? 0 : stable.optInt("versionCode", 0);
            int testCode = test == null ? 0 : test.optInt("versionCode", 0);
            boolean stableUpdate = stableCode > current;
            boolean testUpdate = testCode > current;
            boolean chooseTest = testUpdate && (!stableUpdate || testCode > stableCode);
            JSONObject chosen = chooseTest ? test : (stableUpdate ? stable : null);
            boolean chosenUpdate = chosen != null;
            boolean dot = remindersEnabled(context) && chosenUpdate
                && (!chooseTest || isTestUpdateDotEnabled(context));
            String version = chosen == null
                ? null
                : chosen.optString("version", "") + (chooseTest ? "【测试版】" : "");
            String notes = chosen == null ? null : chosen.optString("notes", "");
            // 回调必须切回主线程：控制台在回调里直接操作 View（红点/文案），
            // 后台线程触碰 UI 会 CalledFromWrongThreadException 闪退。
            new Handler(Looper.getMainLooper()).post(() -> callback.onResult(dot, version, notes, null));
        }).start();
    }

    /** 异步检查更新（网络在主线程外）。autoInstall=true 时发现新版自动下载安装。 */
    public static void check(final Context context, final boolean autoInstall, final Callback callback) {
        new Thread(() -> {
            try {
                int current = currentVersionCode(context);
                JSONObject stable = fetchLatest(UPDATE_URL, current);
                JSONObject test = fetchLatest(TEST_UPDATE_URL, current);
                int stableCode = stable == null ? 0 : stable.optInt("versionCode", 0);
                int testCode = test == null ? 0 : test.optInt("versionCode", 0);
                boolean stableUpdate = stableCode > current;
                boolean testUpdate = testCode > current;
                if (!stableUpdate && !testUpdate) {
                    boolean reachable = stable != null || test != null;
                    String version = BuildConfig.VERSION_NAME;
                    String error = reachable ? null : "检查失败：无法连接更新源";
                    new Handler(Looper.getMainLooper()).post(() -> callback.onResult(false, version, null, error));
                    return;
                }
                boolean chooseTest = testUpdate && (!stableUpdate || testCode > stableCode);
                JSONObject chosen = chooseTest ? test : stable;
                String version = chosen.optString("version", "");
                String notes = chosen.optString("notes", "");
                String apkUrl = releaseApkUrl(chosen, chooseTest ? TEST_UPDATE_BASE : UPDATE_BASE);
                if (chooseTest) {
                    version += "【测试版】";
                    notes = "这是测试版本，可能不稳定。\n\n" + notes;
                }
                boolean hasUpdate = chosen.optInt("versionCode", 0) > current;
                // 测试版只提示、不自动安装，避免稳定用户被静默升级到测试通道。
                if (hasUpdate && autoInstall && !chooseTest && !apkUrl.isEmpty()) {
                    downloadAndInstall(context, apkUrl, version);
                }
                final String finalVersion = version;
                final String finalNotes = notes;
                final boolean finalHasUpdate = hasUpdate;
                new Handler(Looper.getMainLooper()).post(() -> callback.onResult(finalHasUpdate, finalVersion, finalNotes, null));
            } catch (Exception e) {
                new Handler(Looper.getMainLooper()).post(() -> callback.onResult(false, null, null, "检查失败：" + e.getMessage()));
            }
        }).start();
    }

    public static void downloadAndInstall(Context context, String apkUrl, String version) {
        // 远端 version 拼入文件名前做净化，防路径穿越。
        String safeVersion = version.replaceAll("[^A-Za-z0-9._-]", "_");
        String fileName = "coomi-update-" + safeVersion + ".apk";

        DownloadManager.Request request = new DownloadManager.Request(Uri.parse(apkUrl));
        request.setTitle("Coomi " + version);
        request.setDescription("正在下载更新包…");
        request.setNotificationVisibility(DownloadManager.Request.VISIBILITY_VISIBLE_NOTIFY_COMPLETED);
        request.setMimeType("application/vnd.android.package-archive");
        // 下载目标必须放在系统 DownloadManager 服务可写的目录：
        // App 私有目录（files/downloads）会被它以 "Unsupported path" 拒绝（Android 10+）。
        // 用 App 专属外部目录 Android/data/<pkg>/files/Download/。
        File target;
        File externalDir = context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS);
        if (externalDir != null) {
            request.setDestinationInExternalFilesDir(context, Environment.DIRECTORY_DOWNLOADS, fileName);
            target = new File(externalDir, fileName);
        } else {
            File dir = new File(context.getFilesDir(), "downloads");
            if (!dir.isDirectory()) dir.mkdirs();
            request.setDestinationUri(Uri.fromFile(new File(dir, fileName)));
            target = new File(dir, fileName);
        }
        if (target.isFile()) target.delete();

        DownloadManager dm = (DownloadManager) context.getSystemService(Context.DOWNLOAD_SERVICE);
        final long downloadId;
        try {
            downloadId = dm.enqueue(request);
        } catch (Exception e) {
            Toast.makeText(context, "发起更新下载失败：" + e.getMessage(), Toast.LENGTH_LONG).show();
            return;
        }

        // 下载完成后触发安装
        DownloadManager manager = dm;
        BroadcastReceiver receiver = new BroadcastReceiver() {
            @Override
            public void onReceive(Context ctx, Intent intent) {
                long id = intent.getLongExtra(DownloadManager.EXTRA_DOWNLOAD_ID, -1);
                if (id != downloadId) return;
                ctx.unregisterReceiver(this);
                File downloaded = new File(target.getAbsolutePath());
                try {
                    Cursor cursor = manager.query(new DownloadManager.Query().setFilterById(id));
                    int status = -1;
                    if (cursor != null && cursor.moveToFirst()) {
                        int col = cursor.getColumnIndex(DownloadManager.COLUMN_STATUS);
                        if (col >= 0) status = cursor.getInt(col);
                        cursor.close();
                    }
                    if (status != DownloadManager.STATUS_SUCCESSFUL || !downloaded.isFile()) {
                        Toast.makeText(ctx, "更新包下载失败", Toast.LENGTH_LONG).show();
                        return;
                    }
                    // 安装前校验签名与当前安装一致，防止更新源被篡改。
                    if (!signatureMatches(ctx, downloaded)) {
                        Toast.makeText(ctx, "更新包签名校验失败，已取消安装", Toast.LENGTH_LONG).show();
                        return;
                    }
                    Uri uri = FileProvider.getUriForFile(ctx, ctx.getPackageName() + ".fileprovider", downloaded);
                    Intent install = new Intent(Intent.ACTION_VIEW);
                    install.setDataAndType(uri, "application/vnd.android.package-archive");
                    install.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                    install.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                    ctx.startActivity(install);
                } catch (Exception e) {
                    Toast.makeText(ctx, "无法打开安装器：" + e.getMessage(), Toast.LENGTH_LONG).show();
                }
            }
        };
        context.registerReceiver(receiver,
            new android.content.IntentFilter(DownloadManager.ACTION_DOWNLOAD_COMPLETE),
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
                ? Context.RECEIVER_NOT_EXPORTED : 0);
    }

    /** 校验下载的 APK 签名证书与当前安装一致（防 MITM/被篡改的更新包）。
     *  API 28+ 读 v2/v3 证书（getSigningInfo），低版本回退 v1（GET_SIGNATURES）。
     *  之前只用 GET_SIGNATURES，而 AGP 8 默认关闭 v1 签名，导致校验恒失败、永远拒装。 */
    private static boolean signatureMatches(Context context, File apk) {
        try {
            PackageManager pm = context.getPackageManager();
            int flags = Build.VERSION.SDK_INT >= 28
                ? PackageManager.GET_SIGNING_CERTIFICATES
                : PackageManager.GET_SIGNATURES;
            PackageInfo current = pm.getPackageInfo(context.getPackageName(), flags);
            PackageInfo remote = pm.getPackageArchiveInfo(apk.getAbsolutePath(), flags);
            if (current == null || remote == null) return false;
            Signature[] cur = signaturesOf(current);
            Signature[] rem = signaturesOf(remote);
            if (cur == null || rem == null || cur.length == 0 || rem.length == 0) return false;
            return cur[0].toCharsString().equals(rem[0].toCharsString());
        } catch (Exception e) {
            return false;
        }
    }

    @SuppressLint("NewApi")
    private static Signature[] signaturesOf(PackageInfo info) {
        if (Build.VERSION.SDK_INT >= 28 && info.signingInfo != null) {
            return info.signingInfo.getApkContentsSigners();
        }
        return info.signatures;
    }

    /** 供 Dashboard 使用：弹结果对话框。 */
    public static void checkAndPrompt(Context context, Runnable after) {
        check(context, true, (hasUpdate, version, notes, error) -> {
            ((android.app.Activity) context).runOnUiThread(() -> {
                if (error != null) {
                    Toast.makeText(context, error, Toast.LENGTH_LONG).show();
                } else if (!hasUpdate) {
                    Toast.makeText(context, "已是最新版本（" + version + "）", Toast.LENGTH_SHORT).show();
                } else {
                    boolean testVersion = version != null && version.contains("测试版");
                    String message;
                    if (testVersion) {
                        message = "这是测试版本，可能不稳定，正式使用请等待正式更新。\n\n请前往「检查更新」页面选择测试通道手动下载安装。";
                    } else {
                        message = notes == null || notes.isEmpty()
                            ? "正在后台下载更新包，完成后自动弹出安装。"
                            : notes + "\n\n正在后台下载更新包，完成后自动弹出安装。";
                    }
                    new AlertDialog.Builder(context)
                        .setTitle("发现新版本 " + version)
                        .setMessage(message)
                        .setPositiveButton("知道了", null)
                        .show();
                }
                if (after != null) after.run();
            });
        });
    }
}
