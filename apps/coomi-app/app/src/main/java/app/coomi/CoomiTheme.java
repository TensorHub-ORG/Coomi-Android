package app.coomi;

import android.app.Activity;
import android.content.Context;
import android.content.SharedPreferences;
import android.content.res.ColorStateList;
import android.content.res.Configuration;
import android.graphics.Color;
import android.graphics.drawable.BitmapDrawable;
import android.graphics.drawable.ColorDrawable;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.InsetDrawable;
import android.graphics.drawable.LayerDrawable;
import android.graphics.drawable.RippleDrawable;
import android.graphics.drawable.StateListDrawable;
import android.net.Uri;
import android.os.Build;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.widget.CompoundButton;
import android.widget.EditText;
import android.widget.ImageView;
import android.widget.ProgressBar;
import android.widget.SeekBar;
import android.widget.Switch;
import android.widget.TextView;

import androidx.annotation.NonNull;

import com.termux.R;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;

/**
 * 三档主题（跟随系统 / 明亮 / 夜间）的统一入口。
 *
 * 档位存 SharedPreferences（键 {@link #PREF_THEME_MODE}），前端设置页经
 * CoomiAndroid JS 桥与 Dashboard 原生设置共用同一份偏好。
 *
 * 必须在 Activity 的 {@code super.onCreate} 之前调用 {@link #applyTheme}，
 * 否则窗口背景 / 状态栏会在主题切换后闪一下旧色。
 */
public final class CoomiTheme {

    public static final String ACTION_THEME_CHANGED =
        com.termux.shared.termux.TermuxConstants.TERMUX_PACKAGE_NAME + ".action.THEME_CHANGED";

    /** 主题档位：system 跟随系统、light 明亮、dark 夜间。 */
    public static final String MODE_SYSTEM = "system";
    public static final String MODE_LIGHT = "light";
    public static final String MODE_DARK = "dark";
    public static final String MODE_BOOK = "book";
    public static final String MODE_ORANGE = "orange";
    public static final String MODE_INK = "ink";
    public static final String MODE_ABYSS = "abyss";
    public static final String MODE_EMBER = "ember";
    public static final String MODE_CELADON = "celadon";
    public static final String MODE_LINEN = "linen";
    /** 旧版单一终端档位，保留用于兼容已保存的偏好。 */
    public static final String MODE_TERMINAL = "terminal";
    /** 终端主题两版：全等宽字体，黑/白自成一套配色，不跟随系统深浅色。 */
    public static final String MODE_TERMINAL_DARK = "terminal-dark";
    public static final String MODE_TERMINAL_LIGHT = "terminal-light";

    public static final String PREF_THEME_MODE = "coomi.themeMode";
    public static final String PREF_DIGITAL_LIFE_ENABLED = "coomi.digitalLifeEnabled";
    private static final String PREF_NAME = "coomi_settings";
    public static final String SURFACE_CONSOLE = "console";
    public static final String SURFACE_CHAT = "chat";
    private static final String PREF_CUSTOM_ENABLED = "coomi.appearance.custom_enabled";
    private static final String PREF_COLOR_PREFIX = "coomi.appearance.color.";
    private static final String PREF_MASK_PREFIX = "coomi.appearance.mask.";
    private static final String PREF_BACKGROUND_URI_PREFIX = "coomi.appearance.background_uri.";
    private static final String PREF_SAVED_PALETTES = "coomi.appearance.saved_palettes";
    private static final String PREF_ACTIVE_PALETTE = "coomi.appearance.active_palette";
    private static final String PREF_DISPLAY_SCALE = "coomi.appearance.display_scale";
    private static final String PREF_MOTTO_FONT = "coomi.appearance.motto_font";
    public static final int MAX_SAVED_PALETTES = 3;
    private static final String[] COLOR_KEYS = {
        "page", "surface", "fill", "border", "text", "text_secondary", "text_muted",
        "accent", "success", "warning", "danger"
    };
    private static final String[] COLOR_DEFAULTS = {
        "#F4F5F7", "#FFFFFF", "#F5F6F8", "#E7E9EE", "#1B1E23", "#5C6270", "#9096A2",
        "#2D61C6", "#1F9D6B", "#B7791F", "#D9503F"
    };

    private CoomiTheme() {}

    public static boolean isDigitalLifeEnabled(Context context) {
        return preferences(context).getBoolean(PREF_DIGITAL_LIFE_ENABLED, false);
    }

    public static void setDigitalLifeEnabled(Context context, boolean enabled) {
        preferences(context).edit().putBoolean(PREF_DIGITAL_LIFE_ENABLED, enabled).apply();
    }

    /** 当前档位，非法值一律回落 system。 */
    @NonNull
    public static String getMode(Context context) {
        SharedPreferences prefs = context.getSharedPreferences(PREF_NAME, Context.MODE_PRIVATE);
        String mode = prefs.getString(PREF_THEME_MODE, MODE_SYSTEM);
        if (!isValid(mode)) {
            return MODE_SYSTEM;
        }
        return mode;
    }

    /** 保存档位并立即应用系统栏颜色（Activity 已创建后的运行时切换）。 */
    public static void setMode(Context context, String mode) {
        if (!isValid(mode)) {
            return;
        }
        context.getSharedPreferences(PREF_NAME, Context.MODE_PRIVATE)
            .edit().putString(PREF_THEME_MODE, mode).commit();
    }

    /** 按档位 + 系统深浅色计算最终是否深色。 */
    public static boolean isDark(Context context) {
        if (isCustomEnabled(context)) {
            int color = getCustomColor(context, "page");
            double luminance = (0.2126 * Color.red(color) + 0.7152 * Color.green(color)
                + 0.0722 * Color.blue(color)) / 255d;
            return luminance < 0.45;
        }
        String mode = getMode(context);
        if (MODE_TERMINAL.equals(mode) || MODE_TERMINAL_DARK.equals(mode) || MODE_DARK.equals(mode) || MODE_INK.equals(mode) || MODE_ABYSS.equals(mode) || MODE_EMBER.equals(mode)) return true;
        // 终端白版自带明确浅色，不能落到 system 推断。
        if (MODE_TERMINAL_LIGHT.equals(mode)) return false;
        if (!MODE_SYSTEM.equals(mode)) return false;
        int night = context.getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return night == Configuration.UI_MODE_NIGHT_YES;
    }

    private static boolean isValid(String mode) {
        return MODE_SYSTEM.equals(mode) || MODE_LIGHT.equals(mode) || MODE_DARK.equals(mode)
            || MODE_BOOK.equals(mode) || MODE_ORANGE.equals(mode) || MODE_INK.equals(mode)
            || MODE_ABYSS.equals(mode) || MODE_EMBER.equals(mode) || MODE_CELADON.equals(mode)
            || MODE_LINEN.equals(mode) || MODE_TERMINAL.equals(mode)
            || MODE_TERMINAL_DARK.equals(mode) || MODE_TERMINAL_LIGHT.equals(mode);
    }

    /**
     * 常规页面（Launcher / Setup）：夜间用 Theme.Coomi.Night，否则 Theme.Coomi。
     * 必须在 {@code super.onCreate} 之前调用。
     */
    public static void applyTheme(Activity activity) {
        String mode = getMode(activity);
        activity.setTheme(themeResource(mode, isDark(activity), false, false));
    }

    /** 页面底色为灰的变体（Dashboard 等）：对应 Theme.Coomi.Page / Theme.Coomi.Night.Page。 */
    public static void applyPageTheme(Activity activity) {
        String mode = getMode(activity);
        activity.setTheme(themeResource(mode, isDark(activity), true, false));
    }

    /** WebView 宿主闪屏变体：对应 Theme.Coomi.Web / Theme.Coomi.Night.Web。 */
    public static void applyWebTheme(Activity activity) {
        String mode = getMode(activity);
        activity.setTheme(themeResource(mode, isDark(activity), false, true));
    }

    private static int themeResource(String mode, boolean dark, boolean page, boolean web) {
        if (MODE_TERMINAL.equals(mode)) return page ? R.style.Theme_Coomi_Terminal_Page : web ? R.style.Theme_Coomi_Terminal_Web : R.style.Theme_Coomi_Terminal;
        if (MODE_TERMINAL_DARK.equals(mode)) return page ? R.style.Theme_Coomi_TerminalDark_Page : web ? R.style.Theme_Coomi_TerminalDark_Web : R.style.Theme_Coomi_TerminalDark;
        if (MODE_TERMINAL_LIGHT.equals(mode)) return page ? R.style.Theme_Coomi_TerminalLight_Page : web ? R.style.Theme_Coomi_TerminalLight_Web : R.style.Theme_Coomi_TerminalLight;
        if (MODE_INK.equals(mode)) return page ? R.style.Theme_Coomi_Ink_Page : web ? R.style.Theme_Coomi_Ink_Web : R.style.Theme_Coomi_Ink;
        if (MODE_ABYSS.equals(mode)) return page ? R.style.Theme_Coomi_Abyss_Page : web ? R.style.Theme_Coomi_Abyss_Web : R.style.Theme_Coomi_Abyss;
        if (MODE_EMBER.equals(mode)) return page ? R.style.Theme_Coomi_Ember_Page : web ? R.style.Theme_Coomi_Ember_Web : R.style.Theme_Coomi_Ember;
        if (MODE_CELADON.equals(mode)) return page ? R.style.Theme_Coomi_Celadon_Page : web ? R.style.Theme_Coomi_Celadon_Web : R.style.Theme_Coomi_Celadon;
        if (MODE_LINEN.equals(mode)) return page ? R.style.Theme_Coomi_Linen_Page : web ? R.style.Theme_Coomi_Linen_Web : R.style.Theme_Coomi_Linen;
        if (dark) return page ? R.style.Theme_Coomi_Night_Page : web ? R.style.Theme_Coomi_Night_Web : R.style.Theme_Coomi_Night;
        if (MODE_BOOK.equals(mode)) return page ? R.style.Theme_Coomi_Book_Page : web ? R.style.Theme_Coomi_Book_Web : R.style.Theme_Coomi_Book;
        if (MODE_ORANGE.equals(mode)) return page ? R.style.Theme_Coomi_Orange_Page : web ? R.style.Theme_Coomi_Orange_Web : R.style.Theme_Coomi_Orange;
        return page ? R.style.Theme_Coomi_Page : web ? R.style.Theme_Coomi_Web : R.style.Theme_Coomi;
    }

    /**
     * Activity 已创建后的运行时系统栏刷新（setThemeMode 切换档位时调用）。
     * 状态栏颜色与图标跟随 isDark；导航栏也一并处理。
     */
    public static void applySystemBars(Activity activity) {
        applySystemBars(activity, false);
    }

    /** Applies bars using the page background used by dashboard-style screens. */
    public static void applyPageSystemBars(Activity activity) {
        applySystemBars(activity, true);
    }

    private static void applySystemBars(Activity activity, boolean page) {
        boolean dark = isDark(activity);
        Window window = activity.getWindow();
        String mode = getMode(activity);
        int background = isCustomEnabled(activity) ? getCustomColor(activity, page ? "page" : "surface")
            : resolveBackgroundColor(activity, mode, dark, page);
        window.setStatusBarColor(background);
        window.setNavigationBarColor(background);
        View decor = window.getDecorView();
        int flags = decor.getSystemUiVisibility();
        if (dark) {
            decor.setSystemUiVisibility(flags & ~View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR);
        } else {
            decor.setSystemUiVisibility(flags | View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR);
        }
        if (isCustomEnabled(activity)) {
            View content = activity.findViewById(android.R.id.content);
            if (content instanceof ViewGroup && ((ViewGroup) content).getChildCount() > 0) {
                View root = ((ViewGroup) content).getChildAt(0);
                root.setBackgroundColor(getCustomColor(activity, page ? "page" : "surface"));
                applyCustomColors(activity, root);
            }
        }
    }

    public static boolean isCustomEnabled(Context context) {
        return preferences(context).getBoolean(PREF_CUSTOM_ENABLED, false);
    }

    public static void setCustomEnabled(Context context, boolean enabled) {
        SharedPreferences.Editor editor = preferences(context).edit().putBoolean(PREF_CUSTOM_ENABLED, enabled);
        if (!enabled) editor.remove(PREF_ACTIVE_PALETTE);
        editor.apply();
    }

    public static String[] colorKeys() {
        return COLOR_KEYS.clone();
    }

    public static int getCustomColor(Context context, String key) {
        String value = getCustomColorHex(context, key);
        try {
            return Color.parseColor(value);
        } catch (IllegalArgumentException ignored) {
            return Color.parseColor(defaultColor(key));
        }
    }

    public static String getCustomColorHex(Context context, String key) {
        return preferences(context).getString(PREF_COLOR_PREFIX + key, defaultColor(key));
    }

    public static boolean setCustomColor(Context context, String key, String value) {
        if (indexOfColor(key) < 0) return false;
        String normalized = value == null ? "" : value.trim().toUpperCase();
        if (!normalized.startsWith("#")) normalized = "#" + normalized;
        try {
            Color.parseColor(normalized);
        } catch (IllegalArgumentException ignored) {
            return false;
        }
        if (normalized.length() != 7) return false;
        preferences(context).edit()
            .putString(PREF_COLOR_PREFIX + key, normalized)
            .remove(PREF_ACTIVE_PALETTE)
            .apply();
        return true;
    }

    public static List<SavedPalette> getSavedPalettes(Context context) {
        return new ArrayList<>(readSavedPalettes(context));
    }

    public static String getActiveSavedPaletteId(Context context) {
        if (!isCustomEnabled(context)) return "";
        return preferences(context).getString(PREF_ACTIVE_PALETTE, "");
    }

    /** Returns the new palette ID, or null when the three-palette limit has been reached. */
    public static synchronized String saveCurrentPalette(Context context, String requestedName) {
        List<SavedPalette> palettes = readSavedPalettes(context);
        if (palettes.size() >= MAX_SAVED_PALETTES) return null;
        String id = UUID.randomUUID().toString();
        String name = normalizePaletteName(requestedName, palettes.size() + 1);
        palettes.add(new SavedPalette(id, name, captureCurrentColors(context)));
        if (!writeSavedPalettes(context, palettes)) return null;
        preferences(context).edit()
            .putBoolean(PREF_CUSTOM_ENABLED, true)
            .putString(PREF_ACTIVE_PALETTE, id)
            .apply();
        return id;
    }

    public static synchronized boolean applySavedPalette(Context context, String id) {
        for (SavedPalette palette : readSavedPalettes(context)) {
            if (!palette.id.equals(id)) continue;
            SharedPreferences.Editor editor = preferences(context).edit()
                .putBoolean(PREF_CUSTOM_ENABLED, true)
                .putString(PREF_ACTIVE_PALETTE, id);
            for (String key : COLOR_KEYS) editor.putString(PREF_COLOR_PREFIX + key, palette.getColor(key));
            return editor.commit();
        }
        return false;
    }

    public static synchronized boolean renameSavedPalette(Context context, String id, String requestedName) {
        List<SavedPalette> palettes = readSavedPalettes(context);
        for (int index = 0; index < palettes.size(); index++) {
            SavedPalette palette = palettes.get(index);
            if (!palette.id.equals(id)) continue;
            palettes.set(index, new SavedPalette(id, normalizePaletteName(requestedName, index + 1), palette.colors));
            return writeSavedPalettes(context, palettes);
        }
        return false;
    }

    public static synchronized boolean overwriteSavedPalette(Context context, String id) {
        List<SavedPalette> palettes = readSavedPalettes(context);
        for (int index = 0; index < palettes.size(); index++) {
            SavedPalette palette = palettes.get(index);
            if (!palette.id.equals(id)) continue;
            palettes.set(index, new SavedPalette(id, palette.name, captureCurrentColors(context)));
            return writeSavedPalettes(context, palettes);
        }
        return false;
    }

    public static synchronized boolean deleteSavedPalette(Context context, String id) {
        List<SavedPalette> palettes = readSavedPalettes(context);
        boolean removed = false;
        for (int index = palettes.size() - 1; index >= 0; index--) {
            if (palettes.get(index).id.equals(id)) {
                palettes.remove(index);
                removed = true;
            }
        }
        if (!removed || !writeSavedPalettes(context, palettes)) return false;
        if (id.equals(preferences(context).getString(PREF_ACTIVE_PALETTE, ""))) {
            preferences(context).edit().remove(PREF_ACTIVE_PALETTE).apply();
        }
        return true;
    }

    public static void resetAppearance(Context context) {
        SharedPreferences.Editor editor = preferences(context).edit()
            .remove(PREF_CUSTOM_ENABLED)
            .remove(PREF_MASK_PREFIX + SURFACE_CONSOLE)
            .remove(PREF_MASK_PREFIX + SURFACE_CHAT)
            .remove(PREF_BACKGROUND_URI_PREFIX + SURFACE_CONSOLE)
            .remove(PREF_BACKGROUND_URI_PREFIX + SURFACE_CHAT)
            .remove(PREF_DISPLAY_SCALE)
            .remove(PREF_MOTTO_FONT)
            .remove(PREF_ACTIVE_PALETTE);
        for (String key : COLOR_KEYS) editor.remove(PREF_COLOR_PREFIX + key);
        editor.apply();
        removeBackground(context, SURFACE_CONSOLE);
        removeBackground(context, SURFACE_CHAT);
    }

    public static File backgroundFile(Context context, String surface) {
        String safeSurface = SURFACE_CONSOLE.equals(surface) ? SURFACE_CONSOLE : SURFACE_CHAT;
        return new File(new File(context.getFilesDir(), "appearance"), safeSurface + ".jpg");
    }

    public static boolean hasBackground(Context context, String surface) {
        return backgroundFile(context, surface).isFile() || getBackgroundUri(context, surface) != null;
    }

    public static void removeBackground(Context context, String surface) {
        File file = backgroundFile(context, surface);
        if (file.exists()) file.delete();
        preferences(context).edit().remove(PREF_BACKGROUND_URI_PREFIX + surface).apply();
    }

    public static void setBackgroundUri(Context context, String surface, Uri uri) {
        preferences(context).edit()
            .putString(PREF_BACKGROUND_URI_PREFIX + surface, uri == null ? "" : uri.toString())
            .apply();
    }

    public static Uri getBackgroundUri(Context context, String surface) {
        String value = preferences(context).getString(PREF_BACKGROUND_URI_PREFIX + surface, "");
        return value == null || value.isEmpty() ? null : Uri.parse(value);
    }

    public static InputStream openBackground(Context context, String surface) throws Exception {
        File file = backgroundFile(context, surface);
        if (file.isFile()) return new FileInputStream(file);
        Uri uri = getBackgroundUri(context, surface);
        if (uri == null) return null;
        return context.getContentResolver().openInputStream(uri);
    }

    public static String backgroundMimeType(Context context, String surface) {
        if (backgroundFile(context, surface).isFile()) return "image/jpeg";
        Uri uri = getBackgroundUri(context, surface);
        String type = uri == null ? null : context.getContentResolver().getType(uri);
        return type != null && type.startsWith("image/") ? type : "image/*";
    }

    public static int getBackgroundMask(Context context, String surface) {
        int fallback = SURFACE_CONSOLE.equals(surface) ? 82 : 72;
        return preferences(context).getInt(PREF_MASK_PREFIX + surface, fallback);
    }

    public static int getDisplayScale(Context context) {
        return Math.max(75, Math.min(110, preferences(context).getInt(PREF_DISPLAY_SCALE, 100)));
    }

    public static void setDisplayScale(Context context, int percent) {
        preferences(context).edit().putInt(PREF_DISPLAY_SCALE, Math.max(75, Math.min(110, percent))).apply();
    }

    public static String getMottoFont(Context context) {
        return "guofeng".equals(preferences(context).getString(PREF_MOTTO_FONT, "default"))
            ? "guofeng" : "default";
    }

    public static void setMottoFont(Context context, String font) {
        preferences(context).edit().putString(PREF_MOTTO_FONT,
            "guofeng".equals(font) ? "guofeng" : "default").apply();
    }

    public static void setBackgroundMask(Context context, String surface, int value) {
        preferences(context).edit().putInt(PREF_MASK_PREFIX + surface, Math.max(0, Math.min(95, value))).apply();
    }

    public static String appearanceJson(Context context) {
        JSONObject root = new JSONObject();
        JSONObject colors = new JSONObject();
        try {
            root.put("customEnabled", isCustomEnabled(context));
            for (String key : COLOR_KEYS) colors.put(key, getCustomColorHex(context, key));
            root.put("colors", colors);
            root.put("chatBackground", hasBackground(context, SURFACE_CHAT));
            root.put("chatMask", getBackgroundMask(context, SURFACE_CHAT));
            root.put("displayScale", getDisplayScale(context) / 100d);
            root.put("mottoFont", getMottoFont(context));
            File background = backgroundFile(context, SURFACE_CHAT);
            Uri source = getBackgroundUri(context, SURFACE_CHAT);
            root.put("revision", background.exists() ? background.lastModified()
                : source == null ? 0 : source.toString().hashCode());
        } catch (Exception ignored) { }
        return root.toString();
    }

    public static String appearanceSignature(Context context) {
        return getMode(context) + '|' + appearanceJson(context);
    }

    public static void applyConsoleBackground(Activity activity, View root) {
        int pageColor = isCustomEnabled(activity)
            ? getCustomColor(activity, "page")
            : resolvePageColor(activity);
        if (!hasBackground(activity, SURFACE_CONSOLE)) {
            root.setBackgroundColor(pageColor);
            return;
        }
        try (InputStream input = openBackground(activity, SURFACE_CONSOLE)) {
            if (input == null) throw new IllegalStateException("background stream is unavailable");
            BitmapDrawable image = new BitmapDrawable(activity.getResources(), input);
            image.setGravity(Gravity.FILL);
            ColorDrawable overlay = new ColorDrawable(pageColor);
            overlay.setAlpha(Math.round(255f * getBackgroundMask(activity, SURFACE_CONSOLE) / 100f));
            root.setBackground(new LayerDrawable(new Drawable[]{image, overlay}));
        } catch (Exception ignored) {
            root.setBackgroundColor(pageColor);
        }
    }

    /** Applies the saved custom tokens to native views inflated from the regular XML themes. */
    public static void applyCustomColors(Context context, View root) {
        if (!isCustomEnabled(context) || root == null) return;
        NativePalette palette = new NativePalette(context);
        Map<Integer, Integer> tokenMap = buildNativeTokenMap(context, palette);
        applyCustomPaletteToView(context, root, palette, tokenMap);
    }

    private static void applyCustomPaletteToView(
        Context context,
        View view,
        NativePalette palette,
        Map<Integer, Integer> tokenMap
    ) {
        Drawable background = view.getBackground();
        if (background != null) recolorDrawable(background.mutate(), tokenMap);

        Object rawTag = view.getTag();
        String tag = rawTag instanceof String ? (String) rawTag : "";

        if (view instanceof TextView) {
            TextView text = (TextView) view;
            int mapped = mapColor(tokenMap, text.getCurrentTextColor());
            if (mapped != text.getCurrentTextColor()) text.setTextColor(mapped);
            if ("coomi:text".equals(tag)) text.setTextColor(palette.text);
            else if ("coomi:text2".equals(tag)) text.setTextColor(palette.text2);
            else if ("coomi:text3".equals(tag)) text.setTextColor(palette.text3);
        }

        if (view instanceof ImageView) {
            ImageView image = (ImageView) view;
            ColorStateList tint = image.getImageTintList();
            if (tint != null) {
                int mapped = mapColor(tokenMap, tint.getDefaultColor());
                image.setImageTintList(ColorStateList.valueOf(mapped));
            }
        }

        if (view instanceof EditText || "coomi:input".equals(tag)) {
            TextView input = (TextView) view;
            input.setTextColor(palette.text);
            input.setHintTextColor(palette.text3);
            view.setBackground(ripple(context,
                rounded(palette.fill, context.getResources().getDimension(R.dimen.coomi_radius_input),
                    palette.border, dp(context, 1)), palette.accent));
        }

        if (view instanceof CompoundButton) {
            CompoundButton control = (CompoundButton) view;
            control.setButtonTintList(controlTint(palette.accent, palette.text3));
            control.setTextColor(palette.text);
        }
        if (view instanceof Switch) {
            Switch toggle = (Switch) view;
            toggle.setThumbTintList(controlTint(palette.accent, palette.text3));
            toggle.setTrackTintList(controlTint(withAlpha(palette.accent, 0x66), withAlpha(palette.text3, 0x55)));
        }
        if (view instanceof SeekBar) {
            SeekBar seekBar = (SeekBar) view;
            seekBar.setProgressTintList(ColorStateList.valueOf(palette.accent));
            seekBar.setThumbTintList(ColorStateList.valueOf(palette.accent));
        } else if (view instanceof ProgressBar) {
            ((ProgressBar) view).setIndeterminateTintList(ColorStateList.valueOf(palette.accent));
        }

        applySemanticNativeStyle(context, view, tag, palette);

        if (view instanceof ViewGroup) {
            ViewGroup group = (ViewGroup) view;
            for (int index = 0; index < group.getChildCount(); index++) {
                applyCustomPaletteToView(context, group.getChildAt(index), palette, tokenMap);
            }
        }
    }

    private static void applySemanticNativeStyle(Context context, View view, String tag, NativePalette palette) {
        float cardRadius = context.getResources().getDimension(R.dimen.coomi_radius_card);
        float buttonRadius = context.getResources().getDimension(R.dimen.coomi_radius_button);
        if ("coomi:card".equals(tag)) {
            view.setBackground(rounded(palette.surface, cardRadius, Color.TRANSPARENT, 0));
        } else if ("coomi:card-outlined".equals(tag)) {
            view.setBackground(rounded(palette.surface, cardRadius, palette.border, dp(context, 1)));
        } else if ("coomi:divider".equals(tag)) {
            view.setBackgroundColor(palette.border);
        } else if ("coomi:button-primary".equals(tag)) {
            view.setBackground(ripple(context,
                rounded(palette.accent, buttonRadius, Color.TRANSPARENT, 0), palette.textOnAccent));
            setButtonText(view, palette.textOnAccent, palette.surface);
        } else if ("coomi:button-secondary".equals(tag)) {
            view.setBackground(ripple(context,
                rounded(palette.fill, buttonRadius, Color.TRANSPARENT, 0), palette.accent));
            setButtonText(view, palette.text2, palette.surface);
        } else if ("coomi:button-outline".equals(tag)) {
            view.setBackground(ripple(context,
                rounded(palette.surface, buttonRadius, palette.accent, dp(context, 2)), palette.accent));
            setButtonText(view, palette.accent, palette.surface);
        } else if ("coomi:button-danger".equals(tag)) {
            view.setBackground(ripple(context,
                rounded(palette.surface, buttonRadius, palette.danger, dp(context, 2)), palette.danger));
            setButtonText(view, palette.danger, palette.surface);
        } else if ("coomi:button-warning".equals(tag)) {
            view.setBackground(ripple(context,
                rounded(palette.warningSoft, buttonRadius, palette.warning, dp(context, 1)), palette.warning));
            setButtonText(view, palette.warning, palette.surface);
        } else if ("coomi:button-text".equals(tag)) {
            view.setBackground(ripple(context, new ColorDrawable(Color.TRANSPARENT), palette.accent));
            setButtonText(view, palette.accent, palette.surface);
        } else if ("coomi:provider-card".equals(tag)) {
            StateListDrawable states = new StateListDrawable();
            states.addState(new int[]{android.R.attr.state_checked},
                rounded(palette.accentSoft, cardRadius, palette.accent, dp(context, 1)));
            states.addState(new int[]{}, rounded(palette.surface, cardRadius, palette.border, dp(context, 1)));
            view.setBackground(states);
            if (view instanceof TextView) {
                ((TextView) view).setTextColor(palette.text);
                ((TextView) view).setCompoundDrawableTintList(ColorStateList.valueOf(palette.accent));
            }
        }
    }

    private static void setButtonText(View view, int enabledColor, int surface) {
        if (!(view instanceof TextView)) return;
        int[][] states = new int[][]{
            new int[]{android.R.attr.state_enabled},
            new int[]{-android.R.attr.state_enabled}
        };
        int[] colors = new int[]{enabledColor, blend(enabledColor, surface, 0.58f)};
        ((TextView) view).setTextColor(new ColorStateList(states, colors));
    }

    private static ColorStateList controlTint(int active, int inactive) {
        return new ColorStateList(
            new int[][]{
                new int[]{android.R.attr.state_checked, android.R.attr.state_enabled},
                new int[]{android.R.attr.state_enabled},
                new int[]{-android.R.attr.state_enabled}
            },
            new int[]{active, inactive, withAlpha(inactive, 0x66)}
        );
    }

    private static Drawable ripple(Context context, Drawable content, int color) {
        return new RippleDrawable(ColorStateList.valueOf(withAlpha(color, 0x2E)), content, null);
    }

    private static GradientDrawable rounded(int fill, float radius, int stroke, int strokeWidth) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setShape(GradientDrawable.RECTANGLE);
        drawable.setColor(fill);
        drawable.setCornerRadius(radius);
        if (strokeWidth > 0) drawable.setStroke(strokeWidth, stroke);
        return drawable;
    }

    private static void recolorDrawable(Drawable drawable, Map<Integer, Integer> tokenMap) {
        if (drawable instanceof GradientDrawable) {
            GradientDrawable gradient = (GradientDrawable) drawable;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
                int[] colors = gradient.getColors();
                if (colors != null) {
                    int[] mapped = colors.clone();
                    for (int index = 0; index < mapped.length; index++) mapped[index] = mapColor(tokenMap, mapped[index]);
                    gradient.setColors(mapped);
                } else if (gradient.getColor() != null) {
                    gradient.setColor(mapColor(tokenMap, gradient.getColor().getDefaultColor()));
                }
            }
        } else if (drawable instanceof ColorDrawable) {
            ColorDrawable color = (ColorDrawable) drawable;
            color.setColor(mapColor(tokenMap, color.getColor()));
        } else if (drawable instanceof LayerDrawable) {
            LayerDrawable layers = (LayerDrawable) drawable;
            for (int index = 0; index < layers.getNumberOfLayers(); index++) {
                Drawable child = layers.getDrawable(index);
                if (child != null) recolorDrawable(child.mutate(), tokenMap);
            }
        } else if (drawable instanceof InsetDrawable) {
            Drawable child = ((InsetDrawable) drawable).getDrawable();
            if (child != null) recolorDrawable(child.mutate(), tokenMap);
        } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && drawable instanceof StateListDrawable) {
            StateListDrawable states = (StateListDrawable) drawable;
            for (int index = 0; index < states.getStateCount(); index++) {
                Drawable child = states.getStateDrawable(index);
                if (child != null) recolorDrawable(child.mutate(), tokenMap);
            }
        }
    }

    private static Map<Integer, Integer> buildNativeTokenMap(Context context, NativePalette palette) {
        Map<Integer, Integer> colors = new HashMap<>();
        putToken(colors, resolveThemeColor(context, R.attr.coomiPage), palette.page);
        putToken(colors, resolveThemeColor(context, R.attr.coomiCard), palette.surface);
        putToken(colors, resolveThemeColor(context, R.attr.coomiFill), palette.fill);
        putToken(colors, resolveThemeColor(context, R.attr.coomiBorder), palette.border);
        putToken(colors, resolveThemeColor(context, R.attr.coomiDivider), palette.border);
        putToken(colors, resolveThemeColor(context, R.attr.coomiText), palette.text);
        putToken(colors, resolveThemeColor(context, R.attr.coomiText2), palette.text2);
        putToken(colors, resolveThemeColor(context, R.attr.coomiText3), palette.text3);
        putToken(colors, resolveThemeColor(context, R.attr.coomiBlue), palette.accent);
        putToken(colors, resolveThemeColor(context, R.attr.coomiBlueSoft), palette.accentSoft);
        putToken(colors, resolveThemeColor(context, R.attr.coomiOk), palette.success);
        putToken(colors, resolveThemeColor(context, R.attr.coomiOkSoft), palette.successSoft);
        putToken(colors, resolveThemeColor(context, R.attr.coomiDanger), palette.danger);
        putToken(colors, resolveThemeColor(context, R.attr.coomiDangerSoft), palette.dangerSoft);
        putToken(colors, resolveThemeColor(context, R.attr.coomiOrange), palette.warning);
        putToken(colors, resolveThemeColor(context, R.attr.coomiOrangeSoft), palette.warningSoft);
        putToken(colors, resolveThemeColor(context, R.attr.coomiWarnSoft), palette.warningSoft);
        return colors;
    }

    private static void putToken(Map<Integer, Integer> colors, int original, int replacement) {
        if (!colors.containsKey(original)) colors.put(original, replacement);
    }

    private static int mapColor(Map<Integer, Integer> colors, int original) {
        Integer replacement = colors.get(original);
        return replacement == null ? original : replacement;
    }

    /** Shared native palette for overlays, including saved custom themes. */
    public static int overlayColor(Context context, String key, int attribute) {
        if (isCustomEnabled(context)) return getCustomColor(context, key);
        Context themed = new android.view.ContextThemeWrapper(context,
            themeResource(getMode(context), isDark(context), false, false));
        return resolveThemeColor(themed, attribute);
    }

    private static int resolveThemeColor(Context context, int attribute) {
        android.util.TypedValue value = new android.util.TypedValue();
        if (!context.getTheme().resolveAttribute(attribute, value, true)) return Color.TRANSPARENT;
        return value.resourceId != 0 ? context.getColor(value.resourceId) : value.data;
    }

    private static int dp(Context context, int value) {
        return Math.round(value * context.getResources().getDisplayMetrics().density);
    }

    private static int withAlpha(int color, int alpha) {
        return Color.argb(alpha, Color.red(color), Color.green(color), Color.blue(color));
    }

    private static int blend(int first, int second, float secondWeight) {
        float firstWeight = 1f - secondWeight;
        return Color.rgb(
            Math.round(Color.red(first) * firstWeight + Color.red(second) * secondWeight),
            Math.round(Color.green(first) * firstWeight + Color.green(second) * secondWeight),
            Math.round(Color.blue(first) * firstWeight + Color.blue(second) * secondWeight)
        );
    }

    private static int contrastText(int background) {
        double luminance = (Color.red(background) * 299d + Color.green(background) * 587d
            + Color.blue(background) * 114d) / 1000d;
        return luminance > 150d ? Color.rgb(22, 24, 28) : Color.WHITE;
    }

    private static final class NativePalette {
        final int page;
        final int surface;
        final int fill;
        final int border;
        final int text;
        final int text2;
        final int text3;
        final int accent;
        final int accentSoft;
        final int textOnAccent;
        final int success;
        final int successSoft;
        final int warning;
        final int warningSoft;
        final int danger;
        final int dangerSoft;

        NativePalette(Context context) {
            page = getCustomColor(context, "page");
            surface = getCustomColor(context, "surface");
            fill = getCustomColor(context, "fill");
            border = getCustomColor(context, "border");
            text = getCustomColor(context, "text");
            text2 = getCustomColor(context, "text_secondary");
            text3 = getCustomColor(context, "text_muted");
            accent = getCustomColor(context, "accent");
            accentSoft = blend(accent, surface, 0.86f);
            textOnAccent = contrastText(accent);
            success = getCustomColor(context, "success");
            successSoft = blend(success, surface, 0.86f);
            warning = getCustomColor(context, "warning");
            warningSoft = blend(warning, surface, 0.86f);
            danger = getCustomColor(context, "danger");
            dangerSoft = blend(danger, surface, 0.86f);
        }
    }

    private static int resolvePageColor(Context context) {
        String mode = getMode(context);
        if (MODE_TERMINAL.equals(mode)) return android.graphics.Color.BLACK;
        if (MODE_TERMINAL_DARK.equals(mode)) return context.getColor(R.color.coomi_terminal_dark_page);
        if (MODE_TERMINAL_LIGHT.equals(mode)) return context.getColor(R.color.coomi_terminal_light_page);
        if (MODE_INK.equals(mode)) return context.getColor(R.color.coomi_ink_page);
        if (MODE_ABYSS.equals(mode)) return context.getColor(R.color.coomi_abyss_page);
        if (MODE_EMBER.equals(mode)) return context.getColor(R.color.coomi_ember_page);
        if (MODE_CELADON.equals(mode)) return context.getColor(R.color.coomi_celadon_page);
        if (MODE_LINEN.equals(mode)) return context.getColor(R.color.coomi_linen_page);
        if (isDark(context)) return context.getColor(R.color.coomi_night_page);
        if (MODE_BOOK.equals(mode)) return context.getColor(R.color.coomi_book_page);
        if (MODE_ORANGE.equals(mode)) return context.getColor(R.color.coomi_orange_theme_page);
        return context.getColor(R.color.coomi_page);
    }

    private static int resolveBackgroundColor(Context context, String mode, boolean dark, boolean page) {
        if (MODE_TERMINAL.equals(mode)) return android.graphics.Color.BLACK;
        if (MODE_TERMINAL_DARK.equals(mode)) return context.getColor(page ? R.color.coomi_terminal_dark_page : R.color.coomi_terminal_dark_bg);
        if (MODE_TERMINAL_LIGHT.equals(mode)) return context.getColor(page ? R.color.coomi_terminal_light_page : R.color.coomi_terminal_light_bg);
        if (MODE_INK.equals(mode)) return context.getColor(page ? R.color.coomi_ink_page : R.color.coomi_ink_bg);
        if (MODE_ABYSS.equals(mode)) return context.getColor(page ? R.color.coomi_abyss_page : R.color.coomi_abyss_bg);
        if (MODE_EMBER.equals(mode)) return context.getColor(page ? R.color.coomi_ember_page : R.color.coomi_ember_bg);
        if (MODE_CELADON.equals(mode)) return context.getColor(page ? R.color.coomi_celadon_page : R.color.coomi_celadon_bg);
        if (MODE_LINEN.equals(mode)) return context.getColor(page ? R.color.coomi_linen_page : R.color.coomi_linen_bg);
        if (dark) return context.getColor(page ? R.color.coomi_night_page : R.color.coomi_night_bg);
        if (MODE_BOOK.equals(mode)) return context.getColor(page ? R.color.coomi_book_page : R.color.coomi_book_bg);
        if (MODE_ORANGE.equals(mode)) return context.getColor(page ? R.color.coomi_orange_theme_page : R.color.coomi_orange_theme_bg);
        return context.getColor(page ? R.color.coomi_page : R.color.coomi_white);
    }

    private static SharedPreferences preferences(Context context) {
        return context.getSharedPreferences(PREF_NAME, Context.MODE_PRIVATE);
    }

    private static int indexOfColor(String key) {
        for (int index = 0; index < COLOR_KEYS.length; index++) {
            if (COLOR_KEYS[index].equals(key)) return index;
        }
        return -1;
    }

    private static String defaultColor(String key) {
        int index = indexOfColor(key);
        return index < 0 ? "#000000" : COLOR_DEFAULTS[index];
    }

    private static JSONObject captureCurrentColors(Context context) {
        JSONObject colors = new JSONObject();
        for (String key : COLOR_KEYS) {
            try {
                colors.put(key, getCustomColorHex(context, key));
            } catch (Exception ignored) { }
        }
        return colors;
    }

    private static List<SavedPalette> readSavedPalettes(Context context) {
        List<SavedPalette> result = new ArrayList<>();
        String serialized = preferences(context).getString(PREF_SAVED_PALETTES, "[]");
        try {
            JSONArray array = new JSONArray(serialized == null ? "[]" : serialized);
            for (int index = 0; index < array.length() && result.size() < MAX_SAVED_PALETTES; index++) {
                JSONObject item = array.optJSONObject(index);
                if (item == null) continue;
                String id = item.optString("id", "").trim();
                JSONObject colors = item.optJSONObject("colors");
                if (id.isEmpty() || colors == null) continue;
                result.add(new SavedPalette(id, normalizePaletteName(item.optString("name", ""), result.size() + 1), colors));
            }
        } catch (Exception ignored) { }
        return result;
    }

    private static boolean writeSavedPalettes(Context context, List<SavedPalette> palettes) {
        JSONArray array = new JSONArray();
        for (int index = 0; index < palettes.size() && index < MAX_SAVED_PALETTES; index++) {
            array.put(palettes.get(index).toJson());
        }
        return preferences(context).edit().putString(PREF_SAVED_PALETTES, array.toString()).commit();
    }

    private static String normalizePaletteName(String requestedName, int fallbackIndex) {
        String name = requestedName == null ? "" : requestedName.trim();
        if (name.isEmpty()) name = "自定义配色 " + fallbackIndex;
        return name.length() > 24 ? name.substring(0, 24) : name;
    }

    public static final class SavedPalette {
        public final String id;
        public final String name;
        private final JSONObject colors;

        private SavedPalette(String id, String name, JSONObject colors) {
            this.id = id;
            this.name = name;
            this.colors = colors;
        }

        public String getColor(String key) {
            String value = colors.optString(key, defaultColor(key));
            try {
                Color.parseColor(value);
                return value;
            } catch (IllegalArgumentException ignored) {
                return defaultColor(key);
            }
        }

        private JSONObject toJson() {
            JSONObject item = new JSONObject();
            try {
                item.put("id", id);
                item.put("name", name);
                JSONObject savedColors = new JSONObject();
                for (String key : COLOR_KEYS) savedColors.put(key, getColor(key));
                item.put("colors", savedColors);
            } catch (Exception ignored) { }
            return item;
        }
    }
}
