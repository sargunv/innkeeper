// Runs inside the session under app_process (as root, not an app):
//   CLASSPATH=<jar> app_process /system/bin dev.linuxapk.cli.IconDump PKG SIZE...
// Prints, for PKG's launcher activity:
//   component <flattened ComponentName>
//   label <label, one line>
//   category <ApplicationInfo.category>
//   icon <size> <base64 PNG>          (one line per SIZE)
// The icon is the activity's icon (an activity-alias may override the app's),
// loaded at xxxhdpi and drawn by Android itself, so adaptive icons get the
// system mask and vector drawables are rasterized by the framework.
package dev.linuxapk.cli;

import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.content.res.Resources;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.AdaptiveIconDrawable;
import android.graphics.drawable.Drawable;
import android.os.Looper;
import android.util.Base64;
import android.util.DisplayMetrics;
import java.io.ByteArrayOutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.List;

public final class IconDump {
  private IconDump() {}

  private static Context systemContext() throws Exception {
    Looper.prepareMainLooper();
    Class<?> at = Class.forName("android.app.ActivityThread");
    Constructor<?> ctor = at.getDeclaredConstructor();
    ctor.setAccessible(true);
    Object thread = ctor.newInstance();
    Field cur = at.getDeclaredField("sCurrentActivityThread");
    cur.setAccessible(true);
    cur.set(null, thread);
    Method get = at.getDeclaredMethod("getSystemContext");
    get.setAccessible(true);
    return (Context) get.invoke(thread);
  }

  private static String oneLine(CharSequence s) {
    return s == null ? "" : s.toString().replaceAll("[\\r\\n\\t]", " ").trim();
  }

  public static void main(String[] args) throws Exception {
    if (args.length < 1) {
      System.err.println("usage: IconDump PKG SIZE...");
      System.exit(2);
    }
    String pkg = args[0];
    PackageManager pm = systemContext().getPackageManager();
    ApplicationInfo app = pm.getApplicationInfo(pkg, 0);

    Intent main = new Intent(Intent.ACTION_MAIN);
    main.addCategory(Intent.CATEGORY_LAUNCHER);
    main.setPackage(pkg);
    List<ResolveInfo> ris = pm.queryIntentActivities(main, 0);
    ActivityInfo act = ris.isEmpty() ? null : ris.get(0).activityInfo;

    StringBuilder out = new StringBuilder();
    if (act != null) {
      out.append("component ")
          .append(new ComponentName(act.packageName, act.name).flattenToString())
          .append('\n');
    }
    CharSequence label = act != null ? act.loadLabel(pm) : app.loadLabel(pm);
    out.append("label ").append(oneLine(label)).append('\n');
    out.append("category ").append(app.category).append('\n');

    int res = act != null ? act.getIconResource() : app.icon;
    Drawable d = null;
    if (res != 0) {
      try {
        Resources r = pm.getResourcesForApplication(app);
        d = r.getDrawableForDensity(res, DisplayMetrics.DENSITY_XXXHIGH, null);
      } catch (Exception e) {
        System.err.println("IconDump: " + e);
      }
    }
    if (d == null) d = act != null ? act.loadIcon(pm) : app.loadIcon(pm);

    for (int i = 1; i < args.length; i++) {
      int size = Integer.parseInt(args[i]);
      Bitmap bmp = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888);
      Canvas c = new Canvas(bmp);
      // Adaptive icons fill their mask edge to edge; leave a small margin like
      // desktop icon themes do. Legacy icons carry their own padding.
      int pad = d instanceof AdaptiveIconDrawable ? size / 24 : 0;
      d.setBounds(pad, pad, size - pad, size - pad);
      d.draw(c);
      ByteArrayOutputStream png = new ByteArrayOutputStream();
      bmp.compress(Bitmap.CompressFormat.PNG, 100, png);
      out.append("icon ")
          .append(size)
          .append(' ')
          .append(Base64.encodeToString(png.toByteArray(), Base64.NO_WRAP))
          .append('\n');
    }
    System.out.print(out);
    System.out.flush();
    System.exit(0);
  }
}
