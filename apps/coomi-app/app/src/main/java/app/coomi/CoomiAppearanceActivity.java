package app.coomi;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Color;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.RadioButton;
import android.widget.SeekBar;
import android.widget.Switch;
import android.widget.TextView;
import android.widget.Toast;

import com.termux.R;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Theme presets, custom palette, and independent console/chat backgrounds. */
public class CoomiAppearanceActivity extends Activity {
    private static final int REQUEST_CONSOLE_BACKGROUND = 5101;
    private static final int REQUEST_CHAT_BACKGROUND = 5102;
    private static final int MAX_BACKGROUND_EDGE = 2048;

    private final int[] rowIds = {
        R.id.btn_appearance_system, R.id.btn_appearance_light, R.id.btn_appearance_dark,
        R.id.btn_appearance_book, R.id.btn_appearance_orange, R.id.btn_appearance_ink,
        R.id.btn_appearance_abyss, R.id.btn_appearance_ember, R.id.btn_appearance_celadon,
        R.id.btn_appearance_linen, R.id.btn_appearance_terminal_dark, R.id.btn_appearance_terminal_light
    };
    private final int[] radioIds = {
        R.id.radio_appearance_system, R.id.radio_appearance_light, R.id.radio_appearance_dark,
        R.id.radio_appearance_book, R.id.radio_appearance_orange, R.id.radio_appearance_ink,
        R.id.radio_appearance_abyss, R.id.radio_appearance_ember, R.id.radio_appearance_celadon,
        R.id.radio_appearance_linen, R.id.radio_appearance_terminal_dark, R.id.radio_appearance_terminal_light
    };
    private final String[] modes = {"system", "light", "dark", "book", "orange", "ink", "abyss", "ember", "celadon", "linen", "terminal-dark", "terminal-light"};
    private final String[] colorLabels = {
        "页面背景", "内容表面", "控件填充", "边框与分隔线", "主要文字", "次要文字", "弱化文字",
        "强调色", "成功状态", "警告状态", "危险状态"
    };
    private final Map<String, ColorBinding> colorBindings = new LinkedHashMap<>();
    private LinearLayout colorsContainer;
    private Switch customSwitch;
    private View savedPalettesSection;
    private LinearLayout savedPalettesContainer;
    private Button savePaletteButton;
    private TextView consoleBackgroundStatus;
    private TextView chatBackgroundStatus;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        CoomiTheme.applyPageTheme(this);
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_coomi_appearance);
        CoomiTheme.applyPageSystemBars(this);
        findViewById(R.id.btn_appearance_back).setOnClickListener(v -> finish());
        for (int index = 0; index < modes.length; index++) {
            final String mode = modes[index];
            findViewById(rowIds[index]).setOnClickListener(v -> select(mode));
        }
        refreshChecks();
        bindCustomColors();
        bindSavedPalettes();
        bindDisplayScale();
        bindMottoFont();
        bindBackgrounds();
        findViewById(R.id.btn_reset_appearance).setOnClickListener(v -> confirmReset());
        CoomiTheme.applyPageSystemBars(this);
    }

    private void select(String mode) {
        if (CoomiTheme.isCustomEnabled(this)) return;
        if (mode.equals(CoomiTheme.getMode(this)) && !CoomiTheme.isCustomEnabled(this)) return;
        CoomiTheme.setMode(this, mode);
        notifyAppearanceChanged();
        recreate();
    }

    private void refreshChecks() {
        String selected = CoomiTheme.getMode(this);
        boolean customEnabled = CoomiTheme.isCustomEnabled(this);
        for (int index = 0; index < modes.length; index++) {
            View row = findViewById(rowIds[index]);
            RadioButton radio = findViewById(radioIds[index]);
            row.setEnabled(!customEnabled);
            row.setAlpha(customEnabled ? 0.42f : 1f);
            radio.setEnabled(!customEnabled);
            radio.setChecked(!customEnabled && modes[index].equals(selected));
        }
    }

    private void bindMottoFont() {
        TextView value = findViewById(R.id.txt_motto_font);
        value.setText("guofeng".equals(CoomiTheme.getMottoFont(this)) ? "国风" : "默认");
        findViewById(R.id.row_motto_font).setOnClickListener(v ->
            new AlertDialog.Builder(this)
                .setTitle("初始会话界面标语字体")
                .setSingleChoiceItems(new String[]{"默认", "国风"},
                    "guofeng".equals(CoomiTheme.getMottoFont(this)) ? 1 : 0,
                    (dialog, which) -> {
                        CoomiTheme.setMottoFont(this, which == 1 ? "guofeng" : "default");
                        value.setText(which == 1 ? "国风" : "默认");
                        notifyAppearanceChanged();
                        dialog.dismiss();
                    })
                .setNegativeButton("取消", null)
                .show());
    }

    private void bindDisplayScale() {
        SeekBar seek = findViewById(R.id.seek_display_scale);
        TextView value = findViewById(R.id.txt_display_scale);
        int percent = CoomiTheme.getDisplayScale(this);
        seek.setProgress(percent - 75);
        value.setText(percent + "%");
        seek.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override public void onProgressChanged(SeekBar bar, int progress, boolean fromUser) {
                int next = 75 + progress;
                value.setText(next + "%");
                if (fromUser) CoomiTheme.setDisplayScale(CoomiAppearanceActivity.this, next);
            }
            @Override public void onStartTrackingTouch(SeekBar bar) { }
            @Override public void onStopTrackingTouch(SeekBar bar) { notifyAppearanceChanged(); }
        });
    }

    private void bindCustomColors() {
        customSwitch = findViewById(R.id.switch_custom_colors);
        colorsContainer = findViewById(R.id.container_custom_colors);
        String[] keys = CoomiTheme.colorKeys();
        for (int index = 0; index < keys.length; index++) addColorRow(keys[index], colorLabels[index]);
        customSwitch.setChecked(CoomiTheme.isCustomEnabled(this));
        setColorRowsEnabled(customSwitch.isChecked());
        customSwitch.setOnCheckedChangeListener((button, enabled) -> {
            CoomiTheme.setCustomEnabled(this, enabled);
            notifyAppearanceChanged();
            recreate();
        });
    }

    private void addColorRow(String key, String label) {
        View divider = new View(this);
        LinearLayout.LayoutParams dividerParams = new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, dp(1));
        dividerParams.leftMargin = dp(16);
        divider.setLayoutParams(dividerParams);
        divider.setTag("coomi:divider");
        divider.setBackgroundColor(resolveColor(R.attr.coomiDivider));
        colorsContainer.addView(divider);

        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setMinimumHeight(dp(56));
        row.setPadding(dp(16), 0, dp(16), 0);
        row.setClickable(true);
        row.setFocusable(true);
        row.setTag("coomi:row");

        TextView title = new TextView(this);
        title.setText(label);
        title.setTag("coomi:text");
        title.setTextColor(resolveColor(R.attr.coomiText));
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        row.addView(title, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));

        TextView value = new TextView(this);
        value.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        value.setTag("coomi:text3");
        value.setTextColor(resolveColor(R.attr.coomiText3));
        value.setGravity(Gravity.CENTER_VERTICAL | Gravity.END);
        LinearLayout.LayoutParams valueParams = new LinearLayout.LayoutParams(dp(86), dp(40));
        row.addView(value, valueParams);

        View swatch = new View(this);
        LinearLayout.LayoutParams swatchParams = new LinearLayout.LayoutParams(dp(28), dp(28));
        swatchParams.leftMargin = dp(8);
        row.addView(swatch, swatchParams);
        row.setOnClickListener(v -> editColor(key, label));
        colorsContainer.addView(row);
        colorBindings.put(key, new ColorBinding(row, value, swatch));
        refreshColor(key);
    }

    private void editColor(String key, String label) {
        if (!customSwitch.isChecked()) return;
        EditText input = new EditText(this);
        input.setSingleLine(true);
        input.setText(CoomiTheme.getCustomColorHex(this, key));
        input.setSelectAllOnFocus(true);
        int padding = dp(20);
        LinearLayout wrapper = new LinearLayout(this);
        wrapper.setPadding(padding, dp(8), padding, 0);
        wrapper.addView(input, new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        AlertDialog dialog = new AlertDialog.Builder(this)
            .setTitle(label)
            .setMessage("输入 #RRGGBB")
            .setView(wrapper)
            .setNegativeButton("取消", null)
            .setPositiveButton("应用", null)
            .create();
        dialog.setOnShowListener(ignored -> dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(v -> {
            if (!CoomiTheme.setCustomColor(this, key, input.getText().toString())) {
                input.setError("颜色格式无效");
                return;
            }
            refreshColor(key);
            notifyAppearanceChanged();
            dialog.dismiss();
            recreate();
        }));
        dialog.show();
    }

    private void refreshColor(String key) {
        ColorBinding binding = colorBindings.get(key);
        if (binding == null) return;
        String value = CoomiTheme.getCustomColorHex(this, key);
        binding.value.setText(value);
        GradientDrawable swatch = new GradientDrawable();
        swatch.setShape(GradientDrawable.RECTANGLE);
        swatch.setCornerRadius(dp(7));
        swatch.setColor(CoomiTheme.getCustomColor(this, key));
        swatch.setStroke(dp(1), CoomiTheme.isCustomEnabled(this)
            ? CoomiTheme.getCustomColor(this, "border") : resolveColor(R.attr.coomiBorder));
        binding.swatch.setBackground(swatch);
    }

    private void setColorRowsEnabled(boolean enabled) {
        colorsContainer.setAlpha(enabled ? 1f : 0.45f);
        for (ColorBinding binding : colorBindings.values()) binding.row.setEnabled(enabled);
        refreshSavePaletteButton();
    }

    private void bindSavedPalettes() {
        savedPalettesSection = findViewById(R.id.section_saved_palettes);
        savedPalettesContainer = findViewById(R.id.container_saved_palettes);
        savePaletteButton = findViewById(R.id.btn_save_custom_palette);
        savePaletteButton.setOnClickListener(v -> promptSavePalette());
        refreshSavedPalettes();
    }

    private void refreshSavedPalettes() {
        if (savedPalettesContainer == null) return;
        List<CoomiTheme.SavedPalette> palettes = CoomiTheme.getSavedPalettes(this);
        savedPalettesContainer.removeAllViews();
        savedPalettesSection.setVisibility(palettes.isEmpty() ? View.GONE : View.VISIBLE);
        String activeId = CoomiTheme.getActiveSavedPaletteId(this);
        for (int index = 0; index < palettes.size(); index++) {
            if (index > 0) savedPalettesContainer.addView(createDivider());
            addSavedPaletteRow(palettes.get(index), activeId.equals(palettes.get(index).id));
        }
        refreshSavePaletteButton();
        CoomiTheme.applyCustomColors(this, savedPalettesContainer);
    }

    private void refreshSavePaletteButton() {
        if (savePaletteButton == null || customSwitch == null) return;
        int count = CoomiTheme.getSavedPalettes(this).size();
        boolean enabled = customSwitch.isChecked() && count < CoomiTheme.MAX_SAVED_PALETTES;
        savePaletteButton.setText("保存为主题配色（" + count + "/" + CoomiTheme.MAX_SAVED_PALETTES + "）");
        savePaletteButton.setEnabled(enabled);
        savePaletteButton.setAlpha(enabled ? 1f : 0.5f);
    }

    private View createDivider() {
        View divider = new View(this);
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, dp(1));
        params.leftMargin = dp(16);
        divider.setLayoutParams(params);
        divider.setTag("coomi:divider");
        divider.setBackgroundColor(resolveColor(R.attr.coomiDivider));
        return divider;
    }

    private void addSavedPaletteRow(CoomiTheme.SavedPalette palette, boolean active) {
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setMinimumHeight(dp(68));
        row.setPadding(dp(12), 0, dp(8), 0);
        row.setClickable(true);
        row.setFocusable(true);
        row.setTag("coomi:row");
        row.setContentDescription("应用已保存配色 " + palette.name);

        RadioButton radio = new RadioButton(this);
        radio.setChecked(active);
        radio.setClickable(false);
        row.addView(radio, new LinearLayout.LayoutParams(dp(40), LinearLayout.LayoutParams.WRAP_CONTENT));

        LinearLayout details = new LinearLayout(this);
        details.setOrientation(LinearLayout.VERTICAL);
        TextView name = new TextView(this);
        name.setText(palette.name);
        name.setTag("coomi:text");
        name.setTextColor(resolveColor(R.attr.coomiText));
        name.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        name.setSingleLine(true);
        details.addView(name, new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));

        LinearLayout swatches = new LinearLayout(this);
        swatches.setOrientation(LinearLayout.HORIZONTAL);
        swatches.setPadding(0, dp(6), 0, 0);
        for (String key : new String[]{"page", "surface", "accent", "text"}) {
            View swatch = new View(this);
            GradientDrawable background = new GradientDrawable();
            background.setShape(GradientDrawable.OVAL);
            background.setColor(Color.parseColor(palette.getColor(key)));
            background.setStroke(dp(1), resolveColor(R.attr.coomiBorder));
            swatch.setBackground(background);
            LinearLayout.LayoutParams swatchParams = new LinearLayout.LayoutParams(dp(16), dp(16));
            swatchParams.rightMargin = dp(6);
            swatches.addView(swatch, swatchParams);
        }
        details.addView(swatches, new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, dp(22)));
        row.addView(details, new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));

        TextView manage = new TextView(this);
        manage.setText("管理");
        manage.setTag("coomi:button-text");
        manage.setTextColor(resolveColor(R.attr.coomiBlue));
        manage.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14);
        manage.setGravity(Gravity.CENTER);
        manage.setMinWidth(dp(56));
        manage.setMinHeight(dp(48));
        manage.setClickable(true);
        manage.setFocusable(true);
        manage.setContentDescription("管理配色 " + palette.name);
        manage.setOnClickListener(v -> showPaletteActions(palette));
        row.addView(manage, new LinearLayout.LayoutParams(dp(64), dp(48)));

        row.setOnClickListener(v -> {
            if (!CoomiTheme.applySavedPalette(this, palette.id)) {
                Toast.makeText(this, "无法应用该配色", Toast.LENGTH_SHORT).show();
                return;
            }
            notifyAppearanceChanged();
            recreate();
        });
        savedPalettesContainer.addView(row);
    }

    private void promptSavePalette() {
        int count = CoomiTheme.getSavedPalettes(this).size();
        if (!customSwitch.isChecked()) {
            Toast.makeText(this, "请先启用自定义配色", Toast.LENGTH_SHORT).show();
            return;
        }
        if (count >= CoomiTheme.MAX_SAVED_PALETTES) {
            Toast.makeText(this, "最多保存 3 条配色", Toast.LENGTH_SHORT).show();
            return;
        }
        showNameDialog("保存主题配色", "自定义配色 " + (count + 1), name -> {
            if (CoomiTheme.saveCurrentPalette(this, name) == null) {
                Toast.makeText(this, "保存失败或已达到上限", Toast.LENGTH_SHORT).show();
                return;
            }
            refreshChecks();
            refreshSavedPalettes();
            notifyAppearanceChanged();
        });
    }

    private void showPaletteActions(CoomiTheme.SavedPalette palette) {
        new AlertDialog.Builder(this)
            .setTitle(palette.name)
            .setItems(new String[]{"重命名", "用当前配色覆盖", "删除"}, (dialog, which) -> {
                if (which == 0) promptRenamePalette(palette);
                else if (which == 1) confirmOverwritePalette(palette);
                else confirmDeletePalette(palette);
            })
            .setNegativeButton("取消", null)
            .show();
    }

    private void promptRenamePalette(CoomiTheme.SavedPalette palette) {
        showNameDialog("重命名配色", palette.name, name -> {
            if (!CoomiTheme.renameSavedPalette(this, palette.id, name)) {
                Toast.makeText(this, "重命名失败", Toast.LENGTH_SHORT).show();
                return;
            }
            refreshSavedPalettes();
        });
    }

    private void confirmOverwritePalette(CoomiTheme.SavedPalette palette) {
        new AlertDialog.Builder(this)
            .setTitle("覆盖“" + palette.name + "”")
            .setMessage("将使用当前编辑区的完整配色替换此条目。")
            .setNegativeButton("取消", null)
            .setPositiveButton("覆盖", (dialog, which) -> {
                if (!CoomiTheme.overwriteSavedPalette(this, palette.id)) {
                    Toast.makeText(this, "覆盖失败", Toast.LENGTH_SHORT).show();
                    return;
                }
                refreshSavedPalettes();
            })
            .show();
    }

    private void confirmDeletePalette(CoomiTheme.SavedPalette palette) {
        new AlertDialog.Builder(this)
            .setTitle("删除“" + palette.name + "”")
            .setMessage("只删除已保存条目，不会改变当前界面颜色。")
            .setNegativeButton("取消", null)
            .setPositiveButton("删除", (dialog, which) -> {
                if (!CoomiTheme.deleteSavedPalette(this, palette.id)) {
                    Toast.makeText(this, "删除失败", Toast.LENGTH_SHORT).show();
                    return;
                }
                refreshChecks();
                refreshSavedPalettes();
            })
            .show();
    }

    private void showNameDialog(String title, String initialValue, NameCallback callback) {
        EditText input = new EditText(this);
        input.setSingleLine(true);
        input.setMaxLines(1);
        input.setText(initialValue);
        input.setSelectAllOnFocus(true);
        LinearLayout wrapper = new LinearLayout(this);
        wrapper.setPadding(dp(20), dp(8), dp(20), 0);
        wrapper.addView(input, new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        AlertDialog dialog = new AlertDialog.Builder(this)
            .setTitle(title)
            .setView(wrapper)
            .setNegativeButton("取消", null)
            .setPositiveButton("确定", null)
            .create();
        dialog.setOnShowListener(ignored -> dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(v -> {
            String name = input.getText().toString().trim();
            if (name.isEmpty()) {
                input.setError("请输入名称");
                return;
            }
            callback.accept(name);
            dialog.dismiss();
        }));
        dialog.show();
    }

    private void bindBackgrounds() {
        consoleBackgroundStatus = findViewById(R.id.txt_console_background);
        chatBackgroundStatus = findViewById(R.id.txt_chat_background);
        findViewById(R.id.row_console_background).setOnClickListener(v -> pickBackground(REQUEST_CONSOLE_BACKGROUND));
        findViewById(R.id.row_chat_background).setOnClickListener(v -> pickBackground(REQUEST_CHAT_BACKGROUND));
        findViewById(R.id.btn_remove_console_background).setOnClickListener(v -> removeBackground(CoomiTheme.SURFACE_CONSOLE));
        findViewById(R.id.btn_remove_chat_background).setOnClickListener(v -> removeBackground(CoomiTheme.SURFACE_CHAT));
        bindMask((SeekBar) findViewById(R.id.seek_console_mask), CoomiTheme.SURFACE_CONSOLE);
        bindMask((SeekBar) findViewById(R.id.seek_chat_mask), CoomiTheme.SURFACE_CHAT);
        refreshBackgroundStatus();
    }

    private void bindMask(SeekBar seekBar, String surface) {
        seekBar.setProgress(CoomiTheme.getBackgroundMask(this, surface));
        seekBar.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override public void onProgressChanged(SeekBar bar, int value, boolean fromUser) { }
            @Override public void onStartTrackingTouch(SeekBar bar) { }
            @Override public void onStopTrackingTouch(SeekBar bar) {
                CoomiTheme.setBackgroundMask(CoomiAppearanceActivity.this, surface, bar.getProgress());
                notifyAppearanceChanged();
            }
        });
    }

    private void pickBackground(int requestCode) {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("image/*");
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
            | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        startActivityForResult(intent, requestCode);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if ((requestCode != REQUEST_CONSOLE_BACKGROUND && requestCode != REQUEST_CHAT_BACKGROUND)
            || resultCode != RESULT_OK || data == null || data.getData() == null) return;
        String surface = requestCode == REQUEST_CONSOLE_BACKGROUND
            ? CoomiTheme.SURFACE_CONSOLE : CoomiTheme.SURFACE_CHAT;
        Uri uri = data.getData();
        int flags = data.getFlags() & Intent.FLAG_GRANT_READ_URI_PERMISSION;
        try {
            getContentResolver().takePersistableUriPermission(uri, flags);
        } catch (Exception ignored) { }
        CoomiTheme.setBackgroundUri(this, surface, uri);
        refreshBackgroundStatus();
        notifyAppearanceChanged();
        new Thread(() -> {
            String error = copyBackground(uri, surface);
            runOnUiThread(() -> {
                if (error == null) {
                    refreshBackgroundStatus();
                    notifyAppearanceChanged();
                }
            });
        }).start();
    }

    private String copyBackground(Uri uri, String surface) {
        try {
            BitmapFactory.Options bounds = new BitmapFactory.Options();
            bounds.inJustDecodeBounds = true;
            try (InputStream input = getContentResolver().openInputStream(uri)) {
                BitmapFactory.decodeStream(input, null, bounds);
            }
            int sample = 1;
            while (Math.max(bounds.outWidth, bounds.outHeight) / sample > MAX_BACKGROUND_EDGE) sample *= 2;
            BitmapFactory.Options options = new BitmapFactory.Options();
            options.inSampleSize = sample;
            Bitmap bitmap;
            try (InputStream input = getContentResolver().openInputStream(uri)) {
                bitmap = BitmapFactory.decodeStream(input, null, options);
            }
            if (bitmap == null) return "无法读取所选图片";
            File target = CoomiTheme.backgroundFile(this, surface);
            File parent = target.getParentFile();
            if (parent != null && !parent.exists() && !parent.mkdirs()) return "无法创建外观目录";
            File temporary = new File(parent, target.getName() + ".tmp");
            try (FileOutputStream output = new FileOutputStream(temporary)) {
                if (!bitmap.compress(Bitmap.CompressFormat.JPEG, 88, output)) return "无法保存背景图";
            } finally {
                bitmap.recycle();
            }
            if (target.exists() && !target.delete()) return "无法替换旧背景图";
            if (!temporary.renameTo(target)) return "无法写入背景图";
            return null;
        } catch (Exception error) {
            return "优化背景图失败：" + error.getMessage();
        }
    }

    private void removeBackground(String surface) {
        CoomiTheme.removeBackground(this, surface);
        refreshBackgroundStatus();
        notifyAppearanceChanged();
    }

    private void refreshBackgroundStatus() {
        consoleBackgroundStatus.setText(CoomiTheme.hasBackground(this, CoomiTheme.SURFACE_CONSOLE) ? "已设置 · 点击更换" : "未设置 · 点击选择");
        chatBackgroundStatus.setText(CoomiTheme.hasBackground(this, CoomiTheme.SURFACE_CHAT) ? "已设置 · 点击更换" : "未设置 · 点击选择");
    }

    private void confirmReset() {
        new AlertDialog.Builder(this)
            .setTitle("恢复默认外观")
            .setMessage("将关闭自定义配色、移除两张背景图，并恢复默认显示比例和标语字体，主题风格保持不变。")
            .setNegativeButton("取消", null)
            .setPositiveButton("恢复", (dialog, which) -> {
                CoomiTheme.resetAppearance(this);
                notifyAppearanceChanged();
                recreate();
            }).show();
    }

    private void notifyAppearanceChanged() {
        sendBroadcast(new Intent(CoomiTheme.ACTION_THEME_CHANGED).setPackage(getPackageName()));
    }

    private int resolveColor(int attribute) {
        TypedValue value = new TypedValue();
        if (!getTheme().resolveAttribute(attribute, value, true)) return Color.TRANSPARENT;
        return value.resourceId != 0 ? getColor(value.resourceId) : value.data;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private static final class ColorBinding {
        final View row;
        final TextView value;
        final View swatch;
        ColorBinding(View row, TextView value, View swatch) {
            this.row = row;
            this.value = value;
            this.swatch = swatch;
        }
    }

    private interface NameCallback {
        void accept(String name);
    }
}
