// SPDX-License-Identifier: AGPL-3.0-or-later
// Android 17 platform API; compile only, never included in the APK.
package android.os;

public class SystemProperties {
  public static String get(String key) { throw new RuntimeException("stub"); }
  public static String get(String key, String def) { throw new RuntimeException("stub"); }
  public static int getInt(String key, int def) { throw new RuntimeException("stub"); }
  public static boolean getBoolean(String key, boolean def) { throw new RuntimeException("stub"); }
  public static void set(String key, String val) { throw new RuntimeException("stub"); }
}
