// SPDX-License-Identifier: AGPL-3.0-or-later
// Stub of the Android 17 remote insets controller. It is compile-only and stays out of the APK.
package android.view;

import android.content.ComponentName;
import android.view.inputmethod.ImeTracker;

public interface IDisplayWindowInsetsController extends android.os.IInterface {
  void topFocusedWindowChanged(ComponentName component, int requestedVisibleTypes);
  void insetsChanged(InsetsState state);
  void insetsControlChanged(InsetsState state, InsetsSourceControl[] controls);
  void showInsets(int types, ImeTracker.Token token);
  void hideInsets(int types, ImeTracker.Token token);
  void setImeInputTargetRequestedVisibility(boolean visible, ImeTracker.Token token);

  abstract class Stub extends android.os.Binder implements IDisplayWindowInsetsController {
    public android.os.IBinder asBinder() { return this; }
  }
}
