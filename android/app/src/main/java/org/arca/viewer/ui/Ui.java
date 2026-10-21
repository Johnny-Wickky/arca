// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

package org.arca.viewer.ui;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.DialogInterface;
import android.content.res.ColorStateList;
import android.graphics.Typeface;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.RippleDrawable;
import android.text.TextUtils;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import org.arca.viewer.R;

/**
 * View-building helpers: the app's whole UI is built in code from a handful of components, so
 * that spacing, colours, touch targets and typography are decided in one place.
 */
public final class Ui {
    // Text styles.
    public static final int HEADLINE = 0;
    public static final int TITLE = 1;
    public static final int BODY = 2;
    public static final int SMALL = 3;
    public static final int CAPTION = 4;
    public static final int MONO = 5;
    public static final int LABEL = 6;

    // Button kinds.
    public static final int PRIMARY = 0;
    public static final int TONAL = 1;
    public static final int OUTLINE = 2;
    public static final int TEXT_ONLY = 3;
    public static final int DANGER = 4;

    /** Minimum touch target, in dp. */
    public static final int TOUCH = 48;

    public final Context context;
    private final float density;

    public Ui(Context context) {
        this.context = context;
        this.density = context.getResources().getDisplayMetrics().density;
    }

    public int dp(float v) {
        return Math.round(v * density);
    }

    public int color(int res) {
        return context.getColor(res);
    }

    public String str(int res) {
        return context.getString(res);
    }

    public String str(int res, Object... args) {
        return context.getString(res, args);
    }

    // ---- layout helpers ---------------------------------------------------------------------

    public static LinearLayout.LayoutParams lp(int w, int h) {
        return new LinearLayout.LayoutParams(w, h);
    }

    public LinearLayout.LayoutParams wrapLp() {
        return new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    public LinearLayout.LayoutParams fillWidth() {
        return new LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    public LinearLayout.LayoutParams weight(float w) {
        return new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, w);
    }

    public LinearLayout vbox() {
        LinearLayout l = new LinearLayout(context);
        l.setOrientation(LinearLayout.VERTICAL);
        return l;
    }

    public LinearLayout hbox() {
        LinearLayout l = new LinearLayout(context);
        l.setOrientation(LinearLayout.HORIZONTAL);
        l.setGravity(Gravity.CENTER_VERTICAL);
        return l;
    }

    public View space(int dpHeight) {
        View v = new View(context);
        v.setLayoutParams(lp(1, dp(dpHeight)));
        return v;
    }

    public View hspace(int dpWidth) {
        View v = new View(context);
        v.setLayoutParams(lp(dp(dpWidth), 1));
        return v;
    }

    public View divider() {
        View v = new View(context);
        v.setBackgroundColor(color(R.color.arca_outline));
        v.setLayoutParams(lp(ViewGroup.LayoutParams.MATCH_PARENT, Math.max(1, dp(1))));
        return v;
    }

    // ---- text -------------------------------------------------------------------------------

    public void style(TextView t, int style) {
        int colorRes = R.color.arca_on_surface;
        float size;
        Typeface face = Typeface.DEFAULT;
        switch (style) {
            case HEADLINE:
                size = 24;
                face = Typeface.create("sans-serif-medium", Typeface.NORMAL);
                break;
            case TITLE:
                size = 18;
                face = Typeface.create("sans-serif-medium", Typeface.NORMAL);
                break;
            case SMALL:
                size = 14;
                colorRes = R.color.arca_on_surface_variant;
                break;
            case CAPTION:
                size = 12;
                colorRes = R.color.arca_on_surface_variant;
                break;
            case MONO:
                size = 13;
                face = Typeface.MONOSPACE;
                break;
            case LABEL:
                size = 14;
                face = Typeface.create("sans-serif-medium", Typeface.NORMAL);
                break;
            default:
                size = 16;
                break;
        }
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, size);
        t.setTypeface(face);
        t.setTextColor(color(colorRes));
        t.setLineSpacing(0f, style == BODY ? 1.15f : 1.1f);
    }

    public TextView text(CharSequence s, int style) {
        TextView t = new TextView(context);
        t.setText(s);
        style(t, style);
        t.setIncludeFontPadding(true);
        return t;
    }

    public TextView text(int res, int style) {
        return text(context.getString(res), style);
    }

    /** A heading read as such by screen readers. */
    public TextView heading(CharSequence s, int style) {
        TextView t = text(s, style);
        Compat.markHeading(t);
        return t;
    }

    // ---- surfaces ---------------------------------------------------------------------------

    public GradientDrawable rounded(int fill, int stroke, float radiusDp) {
        GradientDrawable g = new GradientDrawable();
        g.setColor(fill);
        g.setCornerRadius(dp(radiusDp));
        if (stroke != 0) {
            g.setStroke(Math.max(1, dp(1)), stroke);
        }
        return g;
    }

    /** Adds a ripple over {@code content}. */
    public Drawable ripple(Drawable content) {
        int base = color(R.color.arca_on_surface);
        int rippleColor = (base & 0x00FFFFFF) | 0x1F000000;
        return new RippleDrawable(ColorStateList.valueOf(rippleColor), content, null);
    }

    /** A rounded card with an outline, padded. */
    public LinearLayout card() {
        LinearLayout l = vbox();
        l.setBackground(rounded(color(R.color.arca_surface), color(R.color.arca_outline), 16));
        int p = dp(16);
        l.setPadding(p, p, p, p);
        return l;
    }

    /** A card with custom fill and no outline (verdict banners). */
    public LinearLayout banner(int fillColorRes) {
        LinearLayout l = vbox();
        l.setBackground(rounded(color(fillColorRes), 0, 20));
        int p = dp(16);
        l.setPadding(p, p, p, p);
        return l;
    }

    public TextView pill(CharSequence s, int fgColorRes, int bgColorRes) {
        TextView t = text(s, LABEL);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 12);
        t.setTextColor(color(fgColorRes));
        t.setBackground(rounded(color(bgColorRes), 0, 12));
        t.setPadding(dp(10), dp(3), dp(10), dp(3));
        t.setSingleLine(true);
        return t;
    }

    public ImageView icon(int drawableRes, int tintColorRes, int sizeDp) {
        ImageView v = new ImageView(context);
        v.setImageDrawable(context.getDrawable(drawableRes));
        v.setImageTintList(ColorStateList.valueOf(color(tintColorRes)));
        v.setLayoutParams(lp(dp(sizeDp), dp(sizeDp)));
        v.setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_NO);
        return v;
    }

    // ---- controls ---------------------------------------------------------------------------

    public Button button(CharSequence label, int kind, int iconRes, View.OnClickListener l) {
        Button b = new Button(context);
        b.setText(label);
        b.setAllCaps(false);
        b.setStateListAnimator(null);
        b.setElevation(0f);
        b.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        b.setTypeface(Typeface.create("sans-serif-medium", Typeface.NORMAL));
        b.setMinHeight(dp(TOUCH));
        b.setMinimumHeight(dp(TOUCH));
        b.setMinWidth(dp(TOUCH));
        b.setMinimumWidth(dp(TOUCH));
        b.setPadding(dp(18), dp(8), dp(18), dp(8));
        b.setGravity(Gravity.CENTER);
        int fill;
        int stroke = 0;
        int fg;
        switch (kind) {
            case PRIMARY:
                fill = color(R.color.arca_primary);
                fg = color(R.color.arca_on_primary);
                break;
            case TONAL:
                fill = color(R.color.arca_primary_container);
                fg = color(R.color.arca_on_primary_container);
                break;
            case DANGER:
                fill = color(R.color.arca_danger_container);
                fg = color(R.color.arca_danger);
                break;
            case OUTLINE:
                fill = 0;
                stroke = color(R.color.arca_outline);
                fg = color(R.color.arca_primary);
                break;
            default:
                fill = 0;
                fg = color(R.color.arca_primary);
                break;
        }
        b.setTextColor(fg);
        b.setBackground(ripple(rounded(fill, stroke, 24)));
        if (iconRes != 0) {
            Drawable d = context.getDrawable(iconRes);
            if (d != null) {
                d = d.mutate();
                d.setTintList(ColorStateList.valueOf(fg));
                d.setBounds(0, 0, dp(20), dp(20));
                b.setCompoundDrawablesRelative(d, null, null, null);
                b.setCompoundDrawablePadding(dp(10));
            }
        }
        b.setOnClickListener(l);
        return b;
    }

    public Button button(int labelRes, int kind, int iconRes, View.OnClickListener l) {
        return button(context.getString(labelRes), kind, iconRes, l);
    }

    public ImageButton iconButton(int drawableRes, int descRes, View.OnClickListener l) {
        ImageButton b = new ImageButton(context);
        b.setImageDrawable(context.getDrawable(drawableRes));
        b.setImageTintList(ColorStateList.valueOf(color(R.color.arca_on_surface)));
        b.setBackground(ripple(rounded(0, 0, 24)));
        b.setContentDescription(context.getString(descRes));
        b.setScaleType(ImageView.ScaleType.CENTER);
        b.setLayoutParams(lp(dp(TOUCH), dp(TOUCH)));
        b.setOnClickListener(l);
        return b;
    }

    /** A selectable chip (tabs, section pickers). */
    public TextView chip(CharSequence label, boolean selected, View.OnClickListener l) {
        TextView t = text(label, LABEL);
        t.setGravity(Gravity.CENTER);
        t.setMinHeight(dp(40));
        t.setPadding(dp(16), dp(8), dp(16), dp(8));
        t.setSingleLine(true);
        t.setEllipsize(TextUtils.TruncateAt.END);
        if (selected) {
            t.setTextColor(color(R.color.arca_on_primary_container));
            t.setBackground(ripple(rounded(color(R.color.arca_primary_container), color(R.color.arca_primary), 20)));
        } else {
            t.setTextColor(color(R.color.arca_on_surface));
            t.setBackground(ripple(rounded(color(R.color.arca_surface_variant), 0, 20)));
        }
        t.setClickable(true);
        t.setFocusable(true);
        t.setOnClickListener(l);
        Compat.markSelected(t, selected);
        return t;
    }

    // ---- dialogs and clipboard --------------------------------------------------------------

    /** Copies text to the clipboard and says so (Android 13+ shows its own confirmation). */
    public void copy(String label, String text) {
        ClipboardManager cm = (ClipboardManager) context.getSystemService(Context.CLIPBOARD_SERVICE);
        if (cm == null) {
            return;
        }
        cm.setPrimaryClip(ClipData.newPlainText(label, text));
        if (android.os.Build.VERSION.SDK_INT < 33) {
            Toast.makeText(context, R.string.copied, Toast.LENGTH_SHORT).show();
        }
    }

    /** Receives the password the reader typed (the array is theirs to wipe). */
    public interface PasswordListener {
        void onPassword(char[] password);
    }

    /**
     * Asks for the password of a document. The field is a password field that is not remembered,
     * suggested or offered to autofill; what was typed is wiped from it as soon as it is read.
     */
    public void askPassword(Activity activity, boolean wrong, final PasswordListener listener) {
        final android.widget.EditText field = new android.widget.EditText(activity);
        field.setInputType(android.text.InputType.TYPE_CLASS_TEXT
            | android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD
            | android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        field.setImeOptions(android.view.inputmethod.EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
            | android.view.inputmethod.EditorInfo.IME_ACTION_DONE);
        field.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);
        field.setHint(R.string.password_hint);
        field.setSingleLine(true);
        LinearLayout box = vbox();
        int p = dp(20);
        box.setPadding(p, dp(8), p, 0);
        if (wrong) {
            TextView err = text(R.string.password_wrong, SMALL);
            err.setTextColor(color(R.color.arca_danger));
            box.addView(err);
        }
        box.addView(field);
        new AlertDialog.Builder(activity)
            .setTitle(R.string.password_dialog_title)
            .setView(box)
            .setPositiveButton(R.string.password_open, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int w) {
                    android.text.Editable e = field.getText();
                    char[] pw = new char[e.length()];
                    e.getChars(0, pw.length, pw, 0);
                    e.clear();
                    if (pw.length > 0) {
                        listener.onPassword(pw);
                    }
                }
            })
            .setNegativeButton(R.string.cancel, null)
            .show();
    }

    /** A dialog with long untrusted text (a link target, say), selectable, with a Copy button. */
    public void showText(Activity activity, CharSequence title, final String value, final String copyLabel) {
        TextView body = text(value, MONO);
        body.setTextIsSelectable(true);
        int p = dp(20);
        body.setPadding(p, dp(8), p, dp(8));
        android.widget.ScrollView sv = new android.widget.ScrollView(context);
        sv.addView(body);
        new AlertDialog.Builder(activity)
            .setTitle(title)
            .setView(sv)
            .setPositiveButton(R.string.copy, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int w) {
                    copy(copyLabel, value);
                }
            })
            .setNegativeButton(R.string.close, null)
            .show();
    }

    /**
     * A dialog for an address from the app's own attribution screen: it shows the full URL and
     * opens it in the browser only after an explicit tap, and only for http(s) addresses.
     */
    public void showLink(final Activity activity, CharSequence title, final String url) {
        final String shown = org.arca.viewer.core.Fmt.safe(url);
        TextView body = text(shown, MONO);
        body.setTextIsSelectable(true);
        int p = dp(20);
        body.setPadding(p, dp(8), p, dp(8));
        android.widget.ScrollView sv = new android.widget.ScrollView(context);
        sv.addView(body);
        AlertDialog.Builder b = new AlertDialog.Builder(activity)
            .setTitle(title)
            .setView(sv)
            .setNeutralButton(R.string.copy, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int w) {
                    copy("link", shown);
                }
            })
            .setNegativeButton(R.string.close, null);
        if (shown.startsWith("https://") || shown.startsWith("http://")) {
            b.setPositiveButton(R.string.open_in_browser, new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface d, int w) {
                    try {
                        android.content.Intent i = new android.content.Intent(android.content.Intent.ACTION_VIEW,
                            android.net.Uri.parse(shown));
                        i.addCategory(android.content.Intent.CATEGORY_BROWSABLE);
                        activity.startActivity(i);
                    } catch (RuntimeException e) {
                        Toast.makeText(context, R.string.no_browser, Toast.LENGTH_LONG).show();
                    }
                }
            });
        }
        b.show();
    }

    /** A plain message dialog. */
    public void showMessage(Activity activity, CharSequence title, CharSequence message) {
        new AlertDialog.Builder(activity)
            .setTitle(title)
            .setMessage(message)
            .setPositiveButton(R.string.close, null)
            .show();
    }

    // ---- containers -------------------------------------------------------------------------

    /** A frame that limits its width and centres its content: readable columns on wide windows. */
    public static final class MaxWidthFrame extends FrameLayout {
        private final int maxWidthPx;

        public MaxWidthFrame(Context c, int maxWidthPx) {
            super(c);
            this.maxWidthPx = maxWidthPx;
        }

        @Override
        protected void onMeasure(int widthSpec, int heightSpec) {
            int w = MeasureSpec.getSize(widthSpec);
            if (w > maxWidthPx) {
                widthSpec = MeasureSpec.makeMeasureSpec(maxWidthPx, MeasureSpec.EXACTLY);
            }
            super.onMeasure(widthSpec, heightSpec);
        }
    }

    /** Wraps {@code content} in a centred column no wider than {@code maxDp}. */
    public View column(View content, int maxDp) {
        FrameLayout outer = new FrameLayout(context);
        MaxWidthFrame inner = new MaxWidthFrame(context, dp(maxDp));
        inner.addView(content, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.WRAP_CONTENT));
        outer.addView(inner, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER_HORIZONTAL));
        return outer;
    }
}
