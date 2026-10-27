// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of a hidden framework interface (see android/hidden/build.gradle.kts).
package android.app;

public interface ITaskStackListener extends android.os.IInterface {
  abstract class Stub extends android.os.Binder implements ITaskStackListener {
    @Override
    public android.os.IBinder asBinder() {
      return this;
    }
  }
}
